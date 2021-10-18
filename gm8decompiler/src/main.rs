use gm8exe::GameVersion;
use log::{error, info, warn};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process,
};

pub mod collision;
pub mod deobfuscate;
pub mod gmk;
pub mod mappings;
pub mod zlib;

#[derive(argh::FromArgs)]
/// GM8 Decompiler extracts the gamedata from a GameMaker8 or GameMaker8.1 exe,
/// then converts it into a .gmk or .gm81 project file to allow editing of the data.
struct Config {
    /// disable various data integrity checks
    #[argh(switch, short = 'l')]
    lazy: bool,

    /// enable verbose logging for decompilation. -v -v is more verbose.
    #[argh(switch, short = 'v')]
    verbose: u8,

    /// set deobfuscation mode auto/on/off (default=auto)
    #[argh(option, short = 'd', default = "deobfuscate::Mode::Auto")]
    deobfuscate: deobfuscate::Mode,

    /// preserve broken events (instead of trying to fix them)
    #[argh(switch, short = 'p')]
    preserve: bool,

    /// decompile gamedata synchronously (lower RAM usage)
    #[argh(switch, short = 's')]
    singlethread: bool,

    /// specify output filename
    #[argh(option, short = 'o')]
    output: Option<String>,

    /// the file to decompile
    #[argh(positional)]
    input: PathBuf,
}

static INFO_STRING: &str = concat!(
    "GM8Decompiler v",
    env!("CARGO_PKG_VERSION"),
    " for ",
    env!("TARGET_TRIPLE"),
    " - built on ",
    env!("BUILD_DATE"),
    ", #",
    env!("GIT_HASH"),
);

fn main() {
    let args: Config = argh::from_env();

    {
        let level = match args.verbose {
            0 => log::LevelFilter::Info,
            1 => log::LevelFilter::Debug,
            _ => log::LevelFilter::Trace,
        };
        env_logger::Builder::new().filter_level(level).init();
    }

    info!("{}", INFO_STRING);

    // print flags for confirmation
    info!("Input file: {}", args.input.display());
    if args.lazy {
        info!("Lazy mode ON: data integrity checking disabled");
    }
    match args.verbose {
        0 => {},
        1 => info!("Verbose logging ON: verbose console output enabled"),
        _ => info!("Verbose logging ON: verbose console output enabled + trace info"),
    };
    match args.deobfuscate {
        deobfuscate::Mode::On => info!("Deobfuscation ON: will standardise GML code"),
        deobfuscate::Mode::Off => info!("Deobfuscation OFF: will ignore obfuscation"),
        _ => (),
    }
    if args.singlethread {
        info!("Single-threaded mode ON: process will not start new threads (slow)");
    }
    if let Some(path) = &args.output {
        info!("Specified output path: {}", path);
    }
    if args.preserve {
        info!("Preserve mode ON: broken events will be preserved and will not be fixed");
    }

    // verify input is usable
    if !args.input.exists() {
        error!("Input '{}' does not exist", args.input.display());
        process::exit(1);
    } else if !args.input.is_file() {
        error!("Input '{}' is not a file.", args.input.display());
        process::exit(1);
    }

    // allow decompile to handle the rest of main
    if let Err(e) =
        decompile(&args.input, args.output, !args.lazy, !args.singlethread, args.deobfuscate, !args.preserve)
    {
        error!("Error parsing gamedata:\n{}", e);
        process::exit(1);
    }
}

fn decompile(
    in_path: &Path,
    out_path: Option<String>,
    strict: bool,
    multithread: bool,
    deobf_mode: deobfuscate::Mode,
    fix_events: bool,
) -> Result<(), String> {
    // slurp in file contents
    let file = fs::read(&in_path).map_err(|e| format!("Failed to read '{}': {}", in_path.display(), e))?;

    // parse (entire) gamedata
    let mut assets = gm8exe::reader::from_exe(file, strict, multithread) // huge call
        .map_err(|e| format!("Reader error: {}", e))?;

    info!("Successfully parsed game!");

    //Do we want to deobfuscate, yes or no?
    let deobfuscate = match deobf_mode {
        deobfuscate::Mode::On => true,
        deobfuscate::Mode::Off => false,
        deobfuscate::Mode::Auto => {
            assets.backgrounds.iter().flatten().any(|s| s.name.0.is_empty())
                || assets.fonts.iter().flatten().any(|s| s.name.0.is_empty())
                || assets.objects.iter().flatten().any(|s| s.name.0.is_empty())
                || assets.paths.iter().flatten().any(|s| s.name.0.is_empty())
                || assets.rooms.iter().flatten().any(|s| s.name.0.is_empty())
                || assets.sounds.iter().flatten().any(|s| s.name.0.is_empty())
                || assets.sprites.iter().flatten().any(|s| s.name.0.is_empty())
                || assets.timelines.iter().flatten().any(|s| s.name.0.is_empty())
        },
    };
    if deobf_mode == deobfuscate::Mode::Auto && deobfuscate {
        warn!("Note: GMK looks obfuscated, so de-obfuscation has been enabled by default");
        warn!(" -- you can turn this off with '-d off'");
    }

    fn fix_event(ev: &mut gm8exe::asset::CodeAction) {
        // So far the only broken event type I know of is custom Execute Code actions.
        // We can fix these by changing the act id and lib id to be a default Execute Code action instead.
        if ev.action_kind == 7 && ev.execution_type == 2 {
            // 7 = code block param, 2 = code execution
            ev.id = 603;
            ev.lib_id = 1;
        }
    }

    if fix_events {
        assets
            .objects
            .iter_mut()
            .flatten()
            .flat_map(|x| x.events.iter_mut().flatten())
            .flat_map(|(_, x)| x.iter_mut())
            .for_each(|ev| fix_event(ev));

        assets
            .timelines
            .iter_mut()
            .flatten()
            .flat_map(|x| x.moments.iter_mut().flat_map(|(_, x)| x.iter_mut()))
            .for_each(|ev| fix_event(ev));
    }

    // warn user if they specified .gmk for 8.0 or .gm81 for 8.0
    let out_expected_ext = match assets.version {
        GameVersion::GameMaker8_0 => "gmk",
        GameVersion::GameMaker8_1 => "gm81",
    };
    let out_path = match out_path {
        Some(p) => {
            let path = PathBuf::from(p);
            match (assets.version, path.extension().and_then(|oss| oss.to_str())) {
                (GameVersion::GameMaker8_0, Some(extension @ "gm81"))
                | (GameVersion::GameMaker8_1, Some(extension @ "gmk")) => {
                    warn!(
                        concat!(
                            "***WARNING*** You've specified an output file '{}'",
                            "a .{} file, for a {} game.\nYou should use '-o {}.{}' instead, ",
                            "otherwise you won't be able to load the file with GameMaker.",
                        ),
                        path.display(),
                        extension,
                        match assets.version {
                            GameVersion::GameMaker8_0 => "GameMaker 8.0",
                            GameVersion::GameMaker8_1 => "GameMaker 8.1",
                        },
                        path.file_stem().and_then(|oss| oss.to_str()).unwrap_or("filename"),
                        out_expected_ext,
                    );
                },
                _ => (),
            }
            path
        },
        None => {
            let mut path = PathBuf::from(in_path);
            path.set_extension(out_expected_ext);
            path
        },
    };

    if deobfuscate {
        deobfuscate::process(&mut assets);
    }

    let mut gmk = fs::File::create(&out_path)
        .map_err(|e| format!("Failed to create output file '{}': {}", out_path.display(), e))?;

    info!("Writing {} header...", out_expected_ext);
    gmk::write_header(&mut gmk, assets.version, assets.game_id, assets.guid)
        .map_err(|e| format!("Failed to write header: {}", e))?;

    info!("Writing {} settings...", out_expected_ext);
    let ico_file = assets.ico_file_raw.take();
    gmk::write_settings(&mut gmk, &assets.settings, ico_file, assets.version)
        .map_err(|e| format!("Failed to write settings block: {}", e))?;

    info!("Writing {} triggers...", assets.triggers.len());
    gmk::write_asset_list(&mut gmk, &assets.triggers, gmk::write_trigger, assets.version, multithread)
        .map_err(|e| format!("Failed to write triggers: {}", e))?;

    gmk::write_timestamp(&mut gmk).map_err(|e| format!("Failed to write timestamp: {}", e))?;

    info!("Writing {} constants...", assets.constants.len());
    gmk::write_constants(&mut gmk, &assets.constants).map_err(|e| format!("Failed to write constants: {}", e))?;

    info!("Writing {} sounds...", assets.sounds.len());
    gmk::write_asset_list(&mut gmk, &assets.sounds, gmk::write_sound, assets.version, multithread)
        .map_err(|e| format!("Failed to write sounds: {}", e))?;

    info!("Writing {} sprites...", assets.sprites.len());
    gmk::write_asset_list(&mut gmk, &assets.sprites, gmk::write_sprite, assets.version, multithread)
        .map_err(|e| format!("Failed to write sprites: {}", e))?;

    info!("Writing {} backgrounds...", assets.backgrounds.len());
    gmk::write_asset_list(&mut gmk, &assets.backgrounds, gmk::write_background, assets.version, multithread)
        .map_err(|e| format!("Failed to write backgrounds: {}", e))?;

    info!("Writing {} paths...", assets.paths.len());
    gmk::write_asset_list(&mut gmk, &assets.paths, gmk::write_path, assets.version, multithread)
        .map_err(|e| format!("Failed to write paths: {}", e))?;

    info!("Writing {} scripts...", assets.scripts.len());
    gmk::write_asset_list(&mut gmk, &assets.scripts, gmk::write_script, assets.version, multithread)
        .map_err(|e| format!("Failed to write scripts: {}", e))?;

    info!("Writing {} fonts...", assets.fonts.len());
    gmk::write_asset_list(&mut gmk, &assets.fonts, gmk::write_font, assets.version, multithread)
        .map_err(|e| format!("Failed to write fonts: {}", e))?;

    info!("Writing {} timelines...", assets.timelines.len());
    gmk::write_asset_list(&mut gmk, &assets.timelines, gmk::write_timeline, assets.version, multithread)
        .map_err(|e| format!("Failed to write timelines: {}", e))?;

    info!("Writing {} objects...", assets.objects.len());
    gmk::write_asset_list(&mut gmk, &assets.objects, gmk::write_object, assets.version, multithread)
        .map_err(|e| format!("Failed to write objects: {}", e))?;

    info!("Writing {} rooms...", assets.rooms.len());
    gmk::write_asset_list(&mut gmk, &assets.rooms, gmk::write_room, assets.version, multithread)
        .map_err(|e| format!("Failed to write rooms: {}", e))?;

    info!(
        "Writing room editor metadata... (last instance: {}, last tile: {})",
        assets.last_instance_id, assets.last_tile_id
    );
    gmk::write_room_editor_meta(&mut gmk, assets.last_instance_id, assets.last_tile_id)
        .map_err(|e| format!("Failed to write room editor metadata: {}", e))?;

    info!("Writing {} included files...", assets.included_files.len());
    gmk::write_included_files(&mut gmk, &assets.included_files)
        .map_err(|e| format!("Failed to write included files: {}", e))?;

    info!("Writing {} extensions...", assets.extensions.len());
    gmk::write_extensions(&mut gmk, &assets.extensions).map_err(|e| format!("Failed to write extensions: {}", e))?;

    info!("Writing game information...");
    gmk::write_game_information(&mut gmk, &assets.help_dialog)
        .map_err(|e| format!("Failed to write game information: {}", e))?;

    info!("Writing {} library initialization strings...", assets.library_init_strings.len());
    gmk::write_library_init_code(&mut gmk, &assets.library_init_strings)
        .map_err(|e| format!("Failed to write library initialization code: {}", e))?;

    info!("Writing room order ({} rooms)...", assets.room_order.len());
    gmk::write_room_order(&mut gmk, &assets.room_order).map_err(|e| format!("Failed to write room order: {}", e))?;

    info!("Writing resource tree...");
    gmk::write_resource_tree(&mut gmk, &assets).map_err(|e| format!("Failed to write resource tree: {}", e))?;

    info!(
        "Successfully written {} to '{}'",
        out_expected_ext,
        out_path.file_name().and_then(|oss| oss.to_str()).unwrap_or("<INVALID UTF-8>"),
    );

    Ok(())
}
