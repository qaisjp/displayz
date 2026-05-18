//! The CLI interface for displayz
//!
//! Use the `--help` flag to see the available options.
use std::cell::RefMut;

use color_eyre::eyre::{Result, eyre};
use displayz::{
    DisplaySettings, Frequency, Orientation, Position, Resolution, query_displays,
    query_stored_topologies, query_topology, read_connectivity_database, refresh,
};
use std::time::{SystemTime, UNIX_EPOCH};
use windows::Win32::Devices::Display::{QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS};
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::Time::FileTimeToSystemTime;
use structopt::{StructOpt, clap::ArgGroup};

/// CLI arguments
#[derive(StructOpt, Debug)]
#[structopt(
    name = "display-cli",
    about = "Allows changing display settings on Windows using the CLI."
)]
struct Opts {
    /// Subcommand to run
    #[structopt(subcommand)]
    cmd: SubCommands,
    /// Output debug info
    #[structopt(short, long, global = true)]
    verbose: bool,
}

/// Subcommands to select the mode of operatiom
#[derive(StructOpt, Debug)]
enum SubCommands {
    /// Displays information about a specific display or all displays if no id is provided
    #[structopt(alias = "i")]
    Info {
        /// The id of the display (optional - if not provided, lists all displays)
        #[structopt(short, long)]
        id: Option<usize>,
        /// Output as JSON
        #[cfg(feature = "json")]
        #[structopt(long)]
        json: bool,
        /// Include inactive (disabled but connected) displays
        #[structopt(long)]
        include_inactive: bool,
        /// Include all possible display paths (implies --include-inactive)
        #[structopt(long)]
        include_all: bool,
        /// Include displays that are not available (e.g. no monitor connected)
        #[structopt(long)]
        include_unavailable: bool,
    },
    /// Sets the primary display
    #[structopt(alias = "sp")]
    SetPrimary {
        #[structopt(short, long)]
        id: usize,
    },
    /// Changes settings of the primary display
    #[structopt(alias = "p")]
    Primary {
        /// The properties to change
        #[structopt(flatten)]
        properties: PropertiesOpt,
    },
    /// Manage display topology
    #[structopt(alias = "t")]
    Topology {
        #[structopt(subcommand)]
        cmd: TopologySubCommands,
    },
    /// Manage display sets (Windows Connectivity database)
    #[structopt(name = "displayset", alias = "ds")]
    DisplaySet {
        #[structopt(subcommand)]
        cmd: DisplaySetSubCommands,
    },
    /// Watch for topology or display set changes (requires admin)
    #[structopt(alias = "w")]
    Watch {
        /// Poll interval in milliseconds
        #[structopt(short, long, default_value = "500")]
        interval: u64,
    },
    /// Changes settings of a display with a specified id
    #[structopt(alias = "props")]
    Properties {
        /// THe id of the display
        #[structopt(short, long)]
        id: usize,
        /// The properties to change
        #[structopt(flatten)]
        properties: PropertiesOpt,
    },
}

/// Subcommands for topology management
#[derive(StructOpt, Debug)]
enum TopologySubCommands {
    /// Show the current topology
    #[structopt(alias = "s")]
    Show,
    /// Show which topology modes are stored for the current display set
    #[structopt(alias = "st")]
    Stored,
}

/// Subcommands for display set management
#[derive(StructOpt, Debug)]
enum DisplaySetSubCommands {
    /// List all entries in the Windows Connectivity database (requires admin)
    #[structopt(alias = "l")]
    List {
        /// Include entries with no matching Configuration or Connectivity key
        #[structopt(long)]
        include_orphaned: bool,
        /// Include entries with no stored topology data (Available: none)
        #[structopt(long)]
        include_empty: bool,
    },
    /// Print the full registry key of the current display set (requires admin)
    #[structopt(alias = "c")]
    Current,
}

fn format_filetime(filetime: u64) -> String {
    let ft = FILETIME {
        dwLowDateTime: (filetime & 0xFFFF_FFFF) as u32,
        dwHighDateTime: (filetime >> 32) as u32,
    };
    let mut st = SYSTEMTIME::default();
    unsafe {
        if FileTimeToSystemTime(&ft, &mut st).is_ok() {
            return format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
            );
        }
    }
    format!("ts:{}", filetime)
}

fn format_time() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

/// Describes the properties that can be changed on a display
#[derive(StructOpt, Debug)]
#[structopt(group = ArgGroup::with_name("prop").required(true).multiple(true))]
struct PropertiesOpt {
    /// Set the position of the display
    #[structopt(
        group = "prop",
        short,
        long,
        long_help = "Set the position of the display. Expected format: `<x>,<y>`"
    )]
    position: Option<Position>,
    /// Sets the resolution of the display
    #[structopt(
        group = "prop",
        short,
        long,
        long_help = "Sets the resolution of the display. Expected format: `<width>x<height>`."
    )]
    resolution: Option<Resolution>,
    // Sets the refresh rate of the display
    #[structopt(
        group = "prop",
        short("t"),
        long,
        long_help = "Sets the refresh rate of the display. Expected format: `<n>`."
    )]
    frequency: Option<Frequency>,
    /// Sets the orientation of the display
    #[structopt(
        group = "prop",
        short,
        long,
        long_help = "Sets the orientation of the display. Expected format: `landscape`, `portrait`, `landscape_flipped`, or `portrait_flipped`."
    )]
    orientation: Option<Orientation>,
}

/// Entry point for `displayz`.
fn main() -> Result<()> {
    color_eyre::install()?;

    let opts = Opts::from_args();

    let log_level = if opts.verbose {
        log::LevelFilter::Trace
    } else {
        log::LevelFilter::Info
    };

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_level.as_str()))
        .init();

    log::debug!("Parsed Opts:\n{:#?}", opts);

    let (qdc_flag, deduplicate) = match &opts.cmd {
        SubCommands::Info { include_all, include_inactive, .. } => {
            if *include_all {
                (QDC_ALL_PATHS, false)
            } else if *include_inactive {
                (QDC_ALL_PATHS, true)
            } else {
                (QDC_ONLY_ACTIVE_PATHS, false)
            }
        }
        _ => (QDC_ONLY_ACTIVE_PATHS, false),
    };

    let display_set = query_displays(qdc_flag, deduplicate)?;
    log::debug!("Discovered displays:\n{}", display_set);

    match opts.cmd {
        SubCommands::Info {
            id,
            #[cfg(feature = "json")]
            json,
            include_unavailable,
            ..
        } => {
            #[cfg(feature = "json")]
            let output_json = json;
            #[cfg(not(feature = "json"))]
            let output_json = false;

            if output_json {
                #[cfg(feature = "json")]
                {
                    use displayz::json;
                    // JSON output
                    match id {
                        Some(id) => {
                            // Display info for a specific display
                            let display = display_set
                                .get(id)
                                .ok_or_else(|| eyre!("Display with id {} not found", id))?;

                            let json_output = json::display_to_json(&display);
                            println!("{}", serde_json::to_string_pretty(&json_output)?);
                        }
                        None => {
                            // List all displays
                            let displays_json: Vec<json::DisplayInfoJson> = display_set
                                .displays()
                                .filter(|d| include_unavailable || d.target_available())
                                .map(|d| json::display_to_json(&d))
                                .collect();
                            println!("{}", serde_json::to_string_pretty(&displays_json)?);
                        }
                    }
                }
            } else {
                // Human-readable output
                match id {
                    Some(id) => {
                        // Display info for a specific display
                        let display = display_set
                            .get(id)
                            .ok_or_else(|| eyre!("Display with id {} not found", id))?;

                        println!("Display ID: {}", display.index());
                        // Windows display number corresponds to the number shown in Windows Settings (Display ID + 1)
                        println!("Windows Display Number: {}", display.index() + 1);
                        println!("Name:       {}", display.name());
                        println!("String:     {}", display.string());
                        println!("Key:        {}", display.key());
                        println!("Primary:    {}", display.is_primary());
                        if let Some(connector) = display.connector_type() {
                            println!("Connector:  {}", connector);
                        }
                        println!("Available:  {}", display.target_available());

                        if let Some(settings) = display.settings() {
                            let settings = settings.borrow();
                            println!("\nSettings:");
                            println!("  Position:          {}", settings.position);
                            println!("  Resolution:        {}", settings.resolution);
                            println!("  Frequency:         {} Hz", settings.frequency);
                            println!("  Orientation:       {}", settings.orientation);
                            println!("  Scaling:           {}", settings.scaling);
                            println!("  Bit Depth:         {}", settings.bit_depth);
                            println!("  Scanline Ordering: {}", settings.scanline_ordering);
                        } else {
                            println!("\nSettings:   None (Inactive)");
                        }
                    }
                    None => {
                        // List all displays
                        println!("All Displays:");
                        println!();
                        for display in display_set.displays().filter(|d| include_unavailable || d.target_available()) {
                            println!("Display ID: {}", display.index());
                            // Windows display number corresponds to the number shown in Windows Settings (Display ID + 1)
                            println!("Windows Display Number: {}", display.index() + 1);
                            println!("Name:       {}", display.name());
                            println!("String:     {}", display.string());
                            println!("Key:        {}", display.key());
                            println!("Primary:    {}", display.is_primary());
                            if let Some(connector) = display.connector_type() {
                                println!("Connector:  {}", connector);
                            }
                            println!("Available:  {}", display.target_available());

                            if let Some(settings) = display.settings() {
                                let settings = settings.borrow();
                                println!("Settings:");
                                println!("  Position:          {}", settings.position);
                                println!("  Resolution:        {}", settings.resolution);
                                println!("  Frequency:         {} Hz", settings.frequency);
                                println!("  Orientation:       {}", settings.orientation);
                                println!("  Scaling:           {}", settings.scaling);
                                println!("  Bit Depth:         {}", settings.bit_depth);
                                println!("  Scanline Ordering: {}", settings.scanline_ordering);
                            } else {
                                println!("Settings:   None (Inactive)");
                            }
                            println!();
                        }
                    }
                }
            }
        }
        SubCommands::SetPrimary { id } => {
            let display = display_set
                .get(id)
                .ok_or_else(|| eyre!("Display with id {} not found", id))?;

            display.set_primary()?;

            display_set.apply()?;
            refresh()?;
            log::info!("Display settings changed");
        }
        SubCommands::Primary { properties } => {
            let display = display_set.primary();

            if let Some(settings) = display.settings() {
                let mut settings = settings.borrow_mut();
                set_properties(&properties, &mut settings);
            } else {
                Err(eyre!("Primary display has no settings"))?;
            }

            display_set.apply()?;
            refresh()?;
            log::info!("Display settings changed");
        }
        SubCommands::Properties { id, properties } => {
            let display = display_set
                .get(id)
                .ok_or_else(|| eyre!("Display with id {} not found", id))?;

            if let Some(settings) = display.settings() {
                let mut settings = settings.borrow_mut();
                set_properties(&properties, &mut settings)
            } else {
                Err(eyre!("Display has no settings"))?;
            }

            display_set.apply()?;
            refresh()?;
            log::info!("Display settings changed");
        }
        SubCommands::Topology { cmd } => match cmd {
            TopologySubCommands::Show => {
                let topology = query_topology()?;
                println!("Topology: {}", topology);
            }
            TopologySubCommands::Stored => {
                let topologies = query_stored_topologies()?;
                if topologies.is_empty() {
                    println!("No stored topologies found for the current display set.");
                } else {
                    println!("Stored topologies for current display set:");
                    for (topology, is_current) in topologies {
                        if is_current {
                            println!("  * {} (current)", topology);
                        } else {
                            println!("    {}", topology);
                        }
                    }
                }
            }
        },
        SubCommands::DisplaySet { cmd } => match cmd {
            DisplaySetSubCommands::List { include_orphaned, include_empty } => {
                // Get current monitor prefixes to identify the active display set
                let all_displays = query_displays(
                    windows::Win32::Devices::Display::QDC_ALL_PATHS,
                    true,
                )?;
                let current_prefixes: Vec<String> = all_displays
                    .displays()
                    .filter(|d| d.target_available())
                    .filter_map(|d| {
                        // key looks like \\?\DISPLAY#DEL430F#...  extract the model code
                        let key = d.key();
                        let after_display = key.split('#').nth(1)?;
                        Some(after_display.to_string())
                    })
                    .collect();

                let is_current = |entry: &displayz::ConnectivityEntry| {
                    let prefixes = entry.monitor_prefixes();
                    prefixes.len() == current_prefixes.len()
                        && prefixes.iter().all(|p| {
                            current_prefixes.iter().any(|c| c.starts_with(p) || p.starts_with(c.as_str()))
                        })
                };

                let max_ts = |e: &displayz::ConnectivityEntry| {
                    [e.internal_timestamp, e.external_timestamp, e.extend_timestamp, e.clone_timestamp]
                        .into_iter().flatten().max().unwrap_or(0)
                };

                let (mut entries, orphaned_configs) = read_connectivity_database()?;
                if entries.is_empty() {
                    println!("No connectivity entries found (try running as administrator).");
                }
                entries.sort_by(|a, b| {
                    is_current(b).cmp(&is_current(a)).then(max_ts(b).cmp(&max_ts(a)))
                });
                for entry in &entries {
                    if !include_empty && entry.available_topologies().is_empty() {
                        continue;
                    }

                    let current = is_current(entry);
                    let marker = if current { " [current set]" } else { "" };
                    println!("Display set:{}", marker);
                    println!("  Monitors:   {}", entry.set_id);
                    println!("  Full key:   {}", entry.key_name);

                    let remembered = entry
                        .recent_topology()
                        .unwrap_or("Unknown");
                    println!("  Remembered: {}", remembered);

                    let topology_fields: &[(&str, &Option<String>, Option<u64>)] = &[
                        ("Internal", &entry.internal, entry.internal_timestamp),
                        ("External", &entry.external, entry.external_timestamp),
                        ("Extend",   &entry.extend,   entry.extend_timestamp),
                        ("Clone",    &entry.clone,    entry.clone_timestamp),
                    ];
                    let available: Vec<String> = topology_fields
                        .iter()
                        .filter(|(_, config_id, _)| config_id.is_some())
                        .map(|(name, _, ts)| {
                            if let Some(ts) = ts {
                                format!("{} [{}]", name, format_filetime(*ts))
                            } else {
                                name.to_string()
                            }
                        })
                        .collect();
                    if available.is_empty() {
                        println!("  Available:  (none)");
                    } else {
                        println!("  Available:  {}", available.join(", "));
                    }
                    if include_orphaned && !entry.has_any_configuration_key() {
                        println!("  Has configuration key: false");
                    }
                    println!();
                }

                if include_orphaned {
                    for orphan in &orphaned_configs {
                        println!("Configuration entry:");
                        println!("  Config ID:  {}", orphan.config_id);
                        println!("  Full key:   {}", orphan.key_name);
                        println!("  Timestamp:  {}", format_filetime(orphan.timestamp));
                        println!("  Has connectivity key: false");
                        println!();
                    }
                }
            }
            DisplaySetSubCommands::Current => {
                let all_displays = query_displays(
                    windows::Win32::Devices::Display::QDC_ALL_PATHS,
                    true,
                )?;
                let current_prefixes: Vec<String> = all_displays
                    .displays()
                    .filter(|d| d.target_available())
                    .filter_map(|d| {
                        let key = d.key();
                        let after_display = key.split('#').nth(1)?;
                        Some(after_display.to_string())
                    })
                    .collect();

                let (entries, _) = read_connectivity_database()?;
                let current = entries.iter().find(|entry| {
                    let prefixes = entry.monitor_prefixes();
                    prefixes.len() == current_prefixes.len()
                        && prefixes.iter().all(|p| {
                            current_prefixes.iter().any(|c| c.starts_with(p) || p.starts_with(c.as_str()))
                        })
                });

                match current {
                    Some(entry) => println!("{}", entry.key_name),
                    None => println!("No matching display set found."),
                }
            }
        },
        SubCommands::Watch { interval } => {
            let current_state = || -> Result<(String, String)> {
                let topology = format!("{}", query_topology()?);

                let all_displays = query_displays(
                    windows::Win32::Devices::Display::QDC_ALL_PATHS,
                    true,
                )?;
                let current_prefixes: Vec<String> = all_displays
                    .displays()
                    .filter(|d| d.target_available())
                    .filter_map(|d| {
                        let key = d.key();
                        let after_display = key.split('#').nth(1)?;
                        Some(after_display.to_string())
                    })
                    .collect();

                let (entries, _) = read_connectivity_database()?;
                let displayset_key = entries
                    .iter()
                    .find(|entry| {
                        let prefixes = entry.monitor_prefixes();
                        prefixes.len() == current_prefixes.len()
                            && prefixes.iter().all(|p| {
                                current_prefixes.iter().any(|c| c.starts_with(p) || p.starts_with(c.as_str()))
                            })
                    })
                    .map(|e| e.key_name.clone())
                    .unwrap_or_else(|| "(none)".to_string());

                Ok((topology, displayset_key))
            };

            let (mut last_topology, mut last_displayset) = current_state()?;
            println!("[{}] Topology: {} | {}", format_time(), last_topology, last_displayset);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(interval));
                let (topology, displayset) = current_state()?;
                if topology != last_topology || displayset != last_displayset {
                    println!("[{}] Topology: {} | {}", format_time(), topology, displayset);
                    last_topology = topology;
                    last_displayset = displayset;
                }
            }
        }
    }

    Ok(())
}

/// Sets a specific settings from the given properties
macro_rules! assign_if_ok {
    ($properties:expr_2021, $settings:expr_2021, $name:ident) => {
        if let Some(value) = $properties.$name {
            $settings.$name = value;
        }
    };
}

/// Sets all available properties
fn set_properties(properties: &PropertiesOpt, settings: &mut RefMut<DisplaySettings>) {
    assign_if_ok!(properties, settings, position);
    assign_if_ok!(properties, settings, resolution);
    assign_if_ok!(properties, settings, frequency);
    assign_if_ok!(properties, settings, orientation);
}
