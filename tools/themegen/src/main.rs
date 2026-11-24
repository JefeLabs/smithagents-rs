use std::path::PathBuf;
use std::process::ExitCode;

use themegen::lint::lint_repo;
use themegen::{check, generate, GenError};

const USAGE: &str = "usage: themegen <generate|check|lint> [--root <dir>]";

fn parse_root(args: &[String]) -> Result<PathBuf, String> {
    match args {
        [] => Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")),
        [flag, dir] if flag == "--root" => Ok(PathBuf::from(dir)),
        _ => Err(USAGE.to_string()),
    }
}

fn report(err: &GenError) -> ExitCode {
    eprintln!("{err}");
    ExitCode::from(1)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if !matches!(cmd.as_str(), "generate" | "check" | "lint") {
        eprintln!("unknown command `{cmd}`\n{USAGE}");
        return ExitCode::from(2);
    }
    let root = match parse_root(rest) {
        Ok(r) => r,
        Err(m) => {
            eprintln!("{m}");
            return ExitCode::from(2);
        }
    };
    let tokens = root.join("design/tokens");
    let out = root.join("design/generated");
    match cmd.as_str() {
        "generate" => match generate(&tokens, &out) {
            Ok(paths) => {
                for p in paths {
                    println!("wrote {}", p.display());
                }
                ExitCode::SUCCESS
            }
            Err(e) => report(&e),
        },
        "check" => match check(&tokens, &out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => report(&e),
        },
        _ => match lint_repo(&root) {
            Ok(findings) if findings.is_empty() => ExitCode::SUCCESS,
            Ok(findings) => {
                for f in findings {
                    let rel = f.path.strip_prefix(&root).unwrap_or(&f.path);
                    println!("{}:{}: {}", rel.display(), f.line, f.text);
                }
                ExitCode::from(1)
            }
            Err(e) => {
                eprintln!("lint failed: {e}");
                ExitCode::from(1)
            }
        },
    }
}
