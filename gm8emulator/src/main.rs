#![feature(seek_stream_len)]

mod action;
mod asset;
mod game;
mod gml;
mod handleman;
mod imgui;
mod input;
mod instance;
mod instancelist;
mod math;
mod render;
mod tile;
mod types;
mod util;

use game::{
    savestate::{self, SaveState},
    Game, PlayType, Replay,
};
use std::{
    env, fs,
    path::PathBuf,
    process,
};
use log::{warn, debug};


#[derive(argh::FromArgs)]
/// GM8 Decompiler extracts the gamedata from a GameMaker8 or GameMaker8.1 exe,
/// then converts it into a .gmk or .gm81 project file to allow editing of the data.
struct Config {
    /// enable various data integrity checks
    #[argh(switch, short = 's')]
    strict: bool,

    /// parse gamedata synchronously
    #[argh(switch, short = 't')]
    singlethread: bool,

    /// enable verbose logging. -v -v is more verbose.
    #[argh(switch, short = 'v')]
    verbose: u8,

    /// disables clock spoofing
    #[argh(switch, short= 'r')]
    realtime: bool,

    /// disables the frame-limiter
    #[argh(switch, short = 'l')]
    no_framelimit: bool,

    /// name of TAS project to create or load
    #[argh(option, short = 'n', from_str_fn(parse_project_name))]
    project_name: Option<PathBuf>,

    /// path to savestate file to replay
    #[argh(option, short = 'f', from_str_fn(parse_replay_file))]
    replay_file: Option<Replay>,
    
    /// output savestate name in replay mode
    #[argh(option, short = 'o')]
    output_file: Option<PathBuf>,

    /// argument to pass to the game
    #[argh(option, short= 'a')]
    game_args: Vec<String>,

    /// the file to decompile
    #[argh(positional)]
    input: PathBuf,
}

fn parse_project_name(value: &str) -> Result<PathBuf, String> {
    let mut p = env::current_dir().map_err(|e| format!("{}", e))?;
    p.push("projects");
    p.push(value);
    Ok(p)
}

fn parse_replay_file(value: &str) -> Result<Replay, String> {
    let filepath = PathBuf::from(&value);
    match filepath.extension().and_then(|x| x.to_str()) {
        Some("bin") => match SaveState::from_file(&filepath, &mut savestate::Buffer::new()) {
            Ok(state) => Ok(state.into_replay()),
            Err(e) => Err(format!("couldn't load {:?}: {:?}", filepath, e)),
        },

        Some("gmtas") => match Replay::from_file(&filepath) {
            Ok(replay) => Ok(replay),
            Err(e) => Err(format!("couldn't load {:?}: {:?}", filepath, e)),
        },

        _ => Err("unknown filetype for -f, expected '.bin' or '.gmtas'".into()),
    }
}

const EXIT_SUCCESS: i32 = 0;
const EXIT_FAILURE: i32 = 1;

fn main() {
    process::exit(xmain());
}

fn xmain() -> i32 {
    let args: Config = { 
        let mut config:Config = argh::from_env();
        config.game_args.insert(0, config.input.clone().into_os_string().into_string().unwrap());
        config
    };

    {
        let level = match args.verbose {
            0 => log::LevelFilter::Info,
            1 => log::LevelFilter::Debug,
            _ => log::LevelFilter::Trace,
        };
        env_logger::Builder::new().filter_level(level).init();
    }

    if let Some(bin) = &args.output_file {
        if bin.extension().and_then(|x| x.to_str()) != Some("bin") {
            eprintln!("invalid output file for -o: must be a .gmtas file");
            return EXIT_FAILURE
        }
    }

    // attempt to find temp dir in project path
    let temp_dir = {
        if let Some(project_name) = &args.project_name {
            let path = std::fs::read_dir(project_name)
            .ok()
            .and_then(|iter| {
                iter.filter_map(|x| x.ok())
                    .find(|p| {
                        p.metadata().ok().filter(|p| p.is_dir()).is_some()
                            && p.file_name().to_str().filter(|s| s.starts_with("gm_ttt_")).is_some()
                    })
                    .map(|entry| entry.path())
            })
            // if we can't find one, make one
            .unwrap_or_else(|| {
                let mut random_int = [0u8; 4];
                getrandom::getrandom(&mut random_int).expect("Couldn't generate a random number");
                let path = [project_name.clone(), format!("gm_ttt_{}", u32::from_le_bytes(random_int) % 100000).into()]
                    .iter()
                    .collect();
                if let Err(e) = std::fs::create_dir_all(&path) {
                    warn!("Could not create temp folder: {}", e);
                    warn!("If this game uses the temp folder, it will most likely crash.");
                }
                path
            });
            Some(path)
        } else {
            None
        }
    };
    let can_clear_temp_dir = temp_dir.is_some();

    let mut file = match fs::read(&args.input) {
        Ok(data) => data,
        Err(err) => {
            eprintln!("failed to open '{}': {}", args.input.display(), err);
            return EXIT_FAILURE
        },
    };

    debug!("loading '{}'...", args.input.display());

    #[rustfmt::skip]
    let assets = gm8exe::reader::from_exe(
        &mut file,                              // mut exe: AsRef<[u8]>
        args.strict,                                 // strict: bool
        !args.singlethread,                            // multithread: bool
    );
    let assets = match assets {
        Ok(assets) => assets,
        Err(err) => {
            eprintln!("failed to load '{}' - {}", args.input.display(), err);
            return EXIT_FAILURE
        },
    };

    let absolute_path = match args.input.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to resolve game path: {}", e);
            return EXIT_FAILURE
        },
    };

    let encoding = encoding_rs::SHIFT_JIS; // TODO: argument

    let play_type = if args.project_name.is_some() {
        PlayType::Record
    } else if args.replay_file.is_some() {
        PlayType::Replay
    } else {
        PlayType::Normal
    };

    let mut components =
        match Game::launch(assets, absolute_path, args.game_args, temp_dir, encoding, !args.no_framelimit, play_type) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("Failed to launch game: {}", e);
                return EXIT_FAILURE
            },
        };

    let time_now = gml::datetime::now_as_nanos();

    if let Err(err) = if let Some(path) = args.project_name {
        components.spoofed_time_nanos = Some(time_now);
        components.record(path);
        Ok(())
    } else {
        // cache temp_dir and included files because the other functions take ownership
        let temp_dir: Option<PathBuf> = if can_clear_temp_dir {
            Some(components.decode_str(components.temp_directory.as_ref()).into_owned().into())
        } else {
            None
        };
        let files_to_delete = components
            .included_files
            .iter()
            .filter(|i| i.remove_at_end)
            .map(|i| PathBuf::from(components.decode_str(i.name.as_ref()).into_owned()))
            .collect::<Vec<_>>();
        let result = if let Some(replay) = args.replay_file {
            components.replay(replay, args.output_file)
        } else {
            components.spoofed_time_nanos = if !args.realtime { Some(time_now) } else { None };
            components.run()
        };
        for file in files_to_delete.into_iter() {
            std::fs::remove_file(file).ok();
        }
        if let Some(temp_dir) = temp_dir {
            std::fs::remove_dir_all(temp_dir).ok();
        }
        result
    } {
        println!("Runtime error: {}", err);
        EXIT_FAILURE
    } else {
        EXIT_SUCCESS
    }
}
