
#[derive(Debug, Serialize)]
pub enum BuildResult {
    Success(Vec<String>),
    Failure(Vec<String>),
    Err
}

// TODO implement
// TODO need to keep a ref to the server to give a build result to.
pub struct BuildQueue;

impl BuildQueue {
    pub fn request_build(&self) {
        unimplemented!();
    }
}

// TODO should be private
pub fn build(build_dir: &str) -> BuildResult {
    use std::env;
    use std::process::Command;

    let mut cmd = Command::new("cargo");
    cmd.arg("rustc");
    cmd.arg("--");
    cmd.arg("-Zno-trans");
    cmd.env("RUSTFLAGS", "-Zunstable-options -Zsave-analysis --error-format=json \
                          -Zcontinue-parse-after-error");
    cmd.env("RUSTC", &env::var("RLS_RUSTC").unwrap_or(String::new()));
    cmd.current_dir(build_dir);
    println!("building {}...", build_dir);
    match cmd.output() {
        Ok(x) => {
            let stderr_json_msg = convert_message_to_json_strings(x.stderr);
            match x.status.code() {
                Some(0) => {
                    BuildResult::Success(stderr_json_msg)
                }
                Some(_) => {
                    BuildResult::Failure(stderr_json_msg)
                }
                None => BuildResult::Err
            }
        }
        Err(_) => {
            BuildResult::Err
        }
    }
}

fn convert_message_to_json_strings(input: Vec<u8>) -> Vec<String> {
    let mut output = vec![];

    //FIXME: this is *so gross*  Trying to work around cargo not supporting json messages
    let it = input.into_iter();

    let mut read_iter = it.skip_while(|&x| x != b'{');

    let mut _msg = String::new();
    loop {
        match read_iter.next() {
            Some(b'\n') => {
                output.push(_msg);
                _msg = String::new();
                while let Some(res) = read_iter.next() {
                    if res == b'{' {
                        _msg.push('{');
                        break;
                    }
                }
            }
            Some(x) => {
                _msg.push(x as char);
            }
            None => {
                break;
            }
        }
    }

    output
}
