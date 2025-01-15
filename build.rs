use std::fs::File;
use std::{env, error, fs, io};
use std::path::Path;
use glob::glob;
use phf::{phf_map, phf_ordered_map};
use tar::Archive;
use temp_dir::TempDir;
use flate2::read::GzDecoder;

type StringResult = Result<String, Box<dyn error::Error>>;

fn get_github_tarball_uri(org: &str, repo: &str, ref_: &str) -> String {
    format!("https://api.github.com/repos/{org}/{repo}/tarball/{ref_}")
}

fn download_tarball(target: &Path, key: &str, uri: &str) -> StringResult {
    let resp = ureq::get(&uri).call()?;
    let wd = TempDir::new()?;
    let path = wd.child(format!("{key}.tar"));
    let mut file = fs::File::create(&path)?;
    io::copy(&mut resp.into_reader(), &mut file)?;

    let mut archive = Archive::new(GzDecoder::new(File::open(path)?));
    archive.unpack(target)?;

    let dir = fs::read_dir(target)?;

    Ok(dir.last().unwrap().unwrap().path().to_str().unwrap().to_string())
}

fn get_github_ref(key: &str, version: &str) -> String {
    match GITHUB_BUILD_DEP_REFS.get(version) {
        Some(refs) => {
            match refs.get(key) {
                Some(x) => x.to_string(),
                None => GITHUB_DEFAULT_BUILD_DEP_REFS.get(key).unwrap().to_string(),
            }
        },
        None => panic!("unsupported version: {version}"),
    }
}

#[derive(Clone)]
enum Dependency {
    GitHub(&'static str, &'static str)
}

impl Dependency {
    fn get_tarball(self, target: &Path, key: &str, version: &str) -> StringResult {
        match self {
            Dependency::GitHub(org, repo) => {
                let ref_ = get_github_ref(key, version);
                let uri = get_github_tarball_uri(org, repo, &ref_);
                download_tarball(target, key, &uri)
            }
        }
    }
}

const BUILD_DEPS: phf::OrderedMap<&str, Dependency> = phf_ordered_map!{
    "envoy"         => Dependency::GitHub("envoyproxy", "envoy"),
    "xds"           => Dependency::GitHub("cncf", "xds"),
    "validate"      => Dependency::GitHub("bufbuild", "protoc-gen-validate"),
    "googleapis"    => Dependency::GitHub("googleapis", "googleapis"),
    "opencensus"    => Dependency::GitHub("census-instrumentation", "opencensus-proto"),
    "opentelemetry" => Dependency::GitHub("open-telemetry", "opentelemetry-proto"),
    "prometheus"    => Dependency::GitHub("prometheus", "client_model"),
};

const GITHUB_BUILD_DEP_REFS: phf::Map<&str, phf::Map<&str, &str>> = phf_map!(
    "1.32" => phf_map!(
        "envoy" => "v1.32.0",
    ),
    "1.31" => phf_map!(
        "envoy" => "v1.31.0",
    ),
    "1.30" => phf_map!(),
);

const GITHUB_DEFAULT_BUILD_DEP_REFS: phf::Map<&str, &str> = phf_map!(
    "envoy"         => "v1.30.0",
    "xds"           => "cff3c89139a3e6a0d4fbddfd158ad895e9b30840",
    "validate"      => "v1.1.1-SNAPSHOT.22",
    "googleapis"    => "b819b9552ddb98c5d2f68719c34b729cfa370fcc",
    "opencensus"    => "v0.4.1",
    "opentelemetry" => "v1.5.0",
    "prometheus"    => "v0.6.1",
);

const BUILD_DEP_DIRS: phf::Map<&str, &str> = phf_map!{
    "opencensus" => "src",
};

fn get_target_dir() -> String {
    match env::var("CARGO_BUILD_TARGET_DIR") {
        Ok(val) => val,
        Err(_) => env::current_dir().unwrap().join("target").to_str().unwrap().to_string(),
    }
}

fn get_api_version() -> String {
    let mut version = "1.32".to_string();
    let mut found = false;

    for (key, _) in env::vars() {
        if key.starts_with("CARGO_FEATURE_API_VERSION_") {
            if found {
                panic!("Multiple version features are not allowed")
            } else {
                found = true;
                version = key.strip_prefix("CARGO_FEATURE_API_VERSION_")
                    .unwrap()
                    .to_string()
                    .replace("_", ".");
            }
        }
    }

    version
}

fn main() {
    let api_version = get_api_version();
    let target_dir = get_target_dir();
    let target_path = Path::new(&target_dir);
    let deps_path = target_path.join("deps");

    let mut protos: Vec<String> = vec![];
    let mut includes: Vec<String> = vec![];
    let mut exclude_comments: Vec<String> = vec![];

    for (key, dep) in BUILD_DEPS.into_iter() {
        let dep_path = deps_path.join(key);
        fs::create_dir_all(&dep_path).unwrap();
        let contents_dir = dep.clone().get_tarball(&dep_path, key, &api_version).unwrap();
        let contents_path = Path::new(&contents_dir);

        if *key == "envoy" {
            let api_path = contents_path.join("api");
            let api_dir = api_path.to_str().unwrap().to_string();
            let mut xds_protos: Vec<String> = glob(&format!("{api_dir}/**/v3/*.proto"))
                .unwrap()
                .filter_map(Result::ok)
                .map(|x| x.to_str().unwrap().to_string())
                .collect();
            protos.append(&mut xds_protos);
            exclude_comments.push(api_dir.clone());
            includes.push(api_dir.clone());
        } else {
            match BUILD_DEP_DIRS.get(&key) {
                Some(subdir) => {
                    let sub_path = contents_path.join(subdir);
                    includes.push(sub_path.to_str().unwrap().to_string());
                },
                None => includes.push(contents_path.to_str().unwrap().to_string()),
            }
        }
    }
     
    env::set_var("PROTOC", protobuf_src::protoc());

    let mut config = prost_build::Config::new();
    config.disable_comments(exclude_comments);

    println!("{config:#?} {protos:#?} {includes:#?}");

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_well_known_types(true)
        .include_file("mod.rs")
        .compile_protos_with_config(
            config,
            &protos,
            &includes,
        ).unwrap();
}
