use std::path::PathBuf;
use std::process::ExitCode;

use smithagents_plugins_tool::{pack, workspace_root, PackOptions};

const USAGE: &str = "usage: cargo plugins pack [--release] [--out <dir>] [--fixtures]";

fn parse(args: &[String]) -> Result<PackOptions, String> {
    let Some((cmd, mut rest)) = args.split_first() else {
        return Err(USAGE.to_string());
    };
    if cmd != "pack" {
        return Err(format!("unknown command `{cmd}`\n{USAGE}"));
    }
    let mut release = false;
    let mut fixtures = false;
    let mut out_dir = None;
    while let Some((flag, tail)) = rest.split_first() {
        rest = tail;
        match flag.as_str() {
            "--release" => release = true,
            "--fixtures" => fixtures = true,
            "--out" => {
                let Some((dir, tail)) = rest.split_first() else {
                    return Err(format!("--out needs a directory\n{USAGE}"));
                };
                out_dir = Some(PathBuf::from(dir));
                rest = tail;
            }
            other => return Err(format!("unknown option `{other}`\n{USAGE}")),
        }
    }
    // By default, beside the daemon binary of the same profile.
    let profile = if release { "release" } else { "debug" };
    let out_dir = out_dir.unwrap_or_else(|| {
        workspace_root()
            .join("target")
            .join(profile)
            .join("plugins")
    });
    Ok(PackOptions {
        release,
        out_dir,
        fixtures,
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse(&args) {
        Ok(opts) => opts,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    eprintln!("building plugins...");
    match pack(&opts) {
        Ok(packed) => {
            for id in &packed.ids {
                println!("packed {}", packed.dir.join(id).display());
            }
            if packed.ids.is_empty() {
                println!("nothing to pack");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(1)
        }
    }
}
