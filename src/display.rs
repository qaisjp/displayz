use core::fmt;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use thiserror::Error;
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_TOPOLOGY_CLONE,
    DISPLAYCONFIG_TOPOLOGY_EXTEND, DISPLAYCONFIG_TOPOLOGY_EXTERNAL,
    DISPLAYCONFIG_TOPOLOGY_ID, DISPLAYCONFIG_TOPOLOGY_INTERNAL, GetDisplayConfigBufferSizes,
    QDC_DATABASE_CURRENT, QDC_ONLY_ACTIVE_PATHS, QUERY_DISPLAY_CONFIG_FLAGS, QueryDisplayConfig,
    SDC_ALLOW_CHANGES, SDC_APPLY, SDC_SAVE_TO_DATABASE, SDC_TOPOLOGY_CLONE, SDC_TOPOLOGY_EXTEND,
    SDC_TOPOLOGY_EXTERNAL, SDC_TOPOLOGY_INTERNAL, SDC_USE_SUPPLIED_DISPLAY_CONFIG, SDC_VALIDATE,
    SetDisplayConfig,
};

use crate::{
    DisplayPropertiesError,
    properties::{DisplayProperties, DisplaySettings},
    types::Position,
};

/// Error type for the display module
#[derive(Error, Debug)]
pub enum DisplayError {
    #[error("Error in DisplayProperties")]
    Properties(#[from] DisplayPropertiesError),
    #[error("Error when calling the Windows API: {0}")]
    WinAPI(String),
    #[error("Only active displays can used as a primary display")]
    PrimaryDisplay,
    #[error("Display {0} has no settings")]
    NoSettings(String),
    #[error("Failed to commit the changes; Returned code: {0}")]
    FailedToCommit(i32),
}

type Result<T = ()> = std::result::Result<T, DisplayError>;

/// A struct that represents a display (index)
#[derive(Debug, Clone)]
pub struct Display<'a> {
    /// The index of the display in the display set
    index: usize,
    /// THe display set containing this display
    display_set: &'a DisplaySet,
}

/// Generates getter for properties of a display
macro_rules! get_properties_str {
    ($field:ident) => {
        pub fn $field(&self) -> &str {
            self.properties().$field.as_str()
        }
    };
}

impl Display<'_> {
    pub fn index(&self) -> usize {
        self.index
    }

    fn properties(&self) -> &DisplayProperties {
        &self.display_set.displays[self.index]
    }

    get_properties_str!(name);
    get_properties_str!(string);
    get_properties_str!(key);

    pub fn settings(&self) -> &Option<RefCell<DisplaySettings>> {
        &self.properties().settings
    }

    pub fn connector_type(&self) -> &Option<crate::types::ConnectorType> {
        &self.properties().connector_type
    }

    pub fn target_available(&self) -> bool {
        self.properties().target_available
    }

    pub fn is_primary(&self) -> bool {
        self.display_set.primary_display.get() == self.index
    }

    pub fn set_primary(&self) -> Result {
        self.display_set.set_primary(self)
    }
}

/// A struct that represents a set of displays
#[derive(Clone)]
pub struct DisplaySet {
    /// The displays in this set
    displays: Vec<DisplayProperties>,
    /// The primary display
    primary_display: Cell<usize>,
    /// The display configuration paths (for modern API)
    paths: RefCell<Vec<DISPLAYCONFIG_PATH_INFO>>,
    /// The display configuration modes (for modern API)
    modes: RefCell<Vec<DISPLAYCONFIG_MODE_INFO>>,
}

impl fmt::Debug for DisplaySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DisplaySet")
            .field("displays", &self.displays)
            .field("primary_display", &self.primary_display)
            .field("paths", &format!("<{} paths>", self.paths.borrow().len()))
            .field("modes", &format!("<{} modes>", self.modes.borrow().len()))
            .finish()
    }
}

impl DisplaySet {
    /// Iterates over the displays in this set
    pub fn displays(&self) -> impl ExactSizeIterator<Item = Display<'_>> {
        self.displays.iter().enumerate().map(|(index, _)| Display {
            index,
            display_set: self,
        })
    }

    /// Returns display for the given `index`
    pub fn get(&self, index: usize) -> Option<Display<'_>> {
        if index >= self.displays.len() {
            return None;
        }
        Some(Display {
            index,
            display_set: self,
        })
    }

    /// Returns the primary display
    pub fn primary(&self) -> Display<'_> {
        Display {
            index: self.primary_display.get(),
            display_set: self,
        }
    }

    /// Sets the given `display` as the primary display
    /// Requires a call to `display_set.apply` and `commit_changes` afterwards
    pub fn set_primary(&self, display: &Display) -> Result {
        let index = display.index;
        let new_primary = &self.displays[index];

        if !new_primary.active {
            return Err(DisplayError::PrimaryDisplay);
        }

        let old_position = new_primary
            .settings
            .as_ref()
            .ok_or_else(|| DisplayError::NoSettings(new_primary.name.to_string()))?
            .borrow()
            .position;

        // move all other displays to new position (because we set a new origin in the next step)
        for (i, display) in self.displays.iter().enumerate() {
            if display.active && i != index {
                let settings = display
                    .settings
                    .as_ref()
                    .ok_or_else(|| DisplayError::NoSettings(display.name.to_string()))?;
                let pos = settings.borrow().position;
                settings.borrow_mut().position = -old_position + pos;
                // unset primary flag on all other displays
                display.primary.set(false);
            }
        }

        // the new primary is the new origin
        let new_primary_mut = &self.displays[index];
        let new_settings = new_primary_mut
            .settings
            .as_ref()
            .ok_or_else(|| DisplayError::NoSettings(new_primary_mut.name.to_string()))?;

        new_settings.borrow_mut().position = Position::new(0, 0);
        // set primary flag on the new primary display
        new_primary_mut.primary.set(true);

        self.primary_display.set(index);

        Ok(())
    }

    /// Applies all pending display configuration changes
    ///
    /// This updates the Windows display configuration to match the current settings.
    /// Modified settings include: position, resolution, frequency, orientation, and scaling.
    /// Read-only properties (bit_depth, scanline_ordering) cannot be changed.
    pub fn apply(&self) -> Result {
        let mut paths = self.paths.borrow_mut();
        let mut modes = self.modes.borrow_mut();

        for display in self.displays.iter().filter(|d| d.active) {
            let Some(path_idx) = Self::find_path_for_display(&paths, &display.name) else {
                continue;
            };

            let path = &mut paths[path_idx];
            let source_idx = unsafe { path.sourceInfo.Anonymous.modeInfoIdx as usize };
            let target_idx = unsafe { path.targetInfo.Anonymous.modeInfoIdx as usize };

            if let Some(settings) = &display.settings {
                let settings = settings.borrow();
                Self::update_source_mode(&mut modes, source_idx, &settings);
                Self::update_target_mode(&mut modes, target_idx, &settings);
                Self::update_path_info(path, &settings);
            }
        }

        Self::commit_display_config(&paths, &modes)
    }

    fn find_path_for_display(
        paths: &[DISPLAYCONFIG_PATH_INFO],
        display_name: &str,
    ) -> Option<usize> {
        paths.iter().position(|p| {
            DisplayProperties::get_source_device_name(p)
                .map(|name| name == display_name)
                .unwrap_or(false)
        })
    }

    fn update_source_mode(
        modes: &mut [DISPLAYCONFIG_MODE_INFO],
        idx: usize,
        settings: &DisplaySettings,
    ) {
        if idx >= modes.len() {
            return;
        }

        unsafe {
            let mode = &mut modes[idx].Anonymous.sourceMode;
            mode.position = settings.position.0;
            mode.width = settings.resolution.width;
            mode.height = settings.resolution.height;
        }
    }

    fn update_target_mode(
        modes: &mut [DISPLAYCONFIG_MODE_INFO],
        idx: usize,
        settings: &DisplaySettings,
    ) {
        if idx >= modes.len() {
            return;
        }

        unsafe {
            let vsync = &mut modes[idx]
                .Anonymous
                .targetMode
                .targetVideoSignalInfo
                .vSyncFreq;
            vsync.Numerator = settings.frequency.0;
            vsync.Denominator = 1;
        }
    }

    fn update_path_info(path: &mut DISPLAYCONFIG_PATH_INFO, settings: &DisplaySettings) {
        use windows::Win32::Devices::Display::{DISPLAYCONFIG_ROTATION, DISPLAYCONFIG_SCALING};

        path.targetInfo.rotation =
            DISPLAYCONFIG_ROTATION(settings.orientation.to_rotation() as i32);
        path.targetInfo.scaling = DISPLAYCONFIG_SCALING(settings.scaling.to_value());
    }

    fn commit_display_config(
        paths: &[DISPLAYCONFIG_PATH_INFO],
        modes: &[DISPLAYCONFIG_MODE_INFO],
    ) -> Result {
        let result = unsafe {
            SetDisplayConfig(
                Some(paths),
                Some(modes),
                SDC_APPLY
                    | SDC_USE_SUPPLIED_DISPLAY_CONFIG
                    | SDC_ALLOW_CHANGES
                    | SDC_SAVE_TO_DATABASE,
            )
        };

        if result == 0 {
            log::debug!("Successfully applied display configuration");
            Ok(())
        } else {
            log::error!(
                "Failed to apply display configuration: error code {}",
                result
            );
            Err(DisplayError::FailedToCommit(result))
        }
    }
}

impl fmt::Display for DisplaySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "DisplaySet {{ displays: [")?;
        for (i, display) in self.displays.iter().enumerate() {
            if i > 0 {
                writeln!(f, ", ")?;
            }
            write!(f, "    {}", display)?;
        }
        write!(f, "\n] }}")
    }
}

/// Returns a list of all displays.
pub fn query_displays(flags: QUERY_DISPLAY_CONFIG_FLAGS, deduplicate: bool) -> Result<DisplaySet> {
    let mut num_paths: u32 = 0;
    let mut num_modes: u32 = 0;

    // Step 1: Get buffer sizes
    unsafe {
        GetDisplayConfigBufferSizes(flags, &mut num_paths, &mut num_modes)
            .ok()
            .map_err(|e| {
                DisplayError::WinAPI(format!("GetDisplayConfigBufferSizes failed: {:?}", e))
            })?;
    }

    log::debug!("Display config: {} paths, {} modes", num_paths, num_modes);

    // Step 2: Allocate and query paths/modes
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); num_paths as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); num_modes as usize];

    // QDC_DATABASE_CURRENT requires a non-null pCurrentTopologyId; all other flags require None.
    let mut topology_id = DISPLAYCONFIG_TOPOLOGY_ID::default();
    let p_topology_id = if flags == QDC_DATABASE_CURRENT {
        Some(&mut topology_id as *mut DISPLAYCONFIG_TOPOLOGY_ID)
    } else {
        None
    };

    unsafe {
        QueryDisplayConfig(
            flags,
            &mut num_paths,
            paths.as_mut_ptr(),
            &mut num_modes,
            modes.as_mut_ptr(),
            p_topology_id,
        )
        .ok()
        .map_err(|e| DisplayError::WinAPI(format!("QueryDisplayConfig failed: {:?}", e)))?;
    }

    // Truncate to actual returned counts
    paths.truncate(num_paths as usize);
    modes.truncate(num_modes as usize);

    // When deduplicating, keep one path per physical monitor (adapterId + targetId),
    // preferring the active path so its settings are available.
    if deduplicate {
        let mut best: HashMap<(u32, i32, u32), usize> = HashMap::new();
        for (i, path) in paths.iter().enumerate() {
            let key = (
                path.targetInfo.adapterId.LowPart,
                path.targetInfo.adapterId.HighPart,
                path.targetInfo.id,
            );
            let active = (path.flags & 0x00000001) != 0;
            match best.entry(key) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(i);
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if active && (paths[*e.get()].flags & 0x00000001) == 0 {
                        e.insert(i);
                    }
                }
            }
        }
        let mut indices: Vec<usize> = best.into_values().collect();
        indices.sort();
        paths = indices.into_iter().map(|i| paths[i]).collect();
    }

    // Step 3: Convert each path to DisplayProperties
    let mut result = Vec::<DisplayProperties>::new();
    let mut primary_index = 0;

    for (path_idx, path) in paths.iter().enumerate() {
        let properties = DisplayProperties::from_display_config(path, &modes)?;

        log::debug!(
            "Display {}: {} - {} (primary={})",
            path_idx,
            properties.name,
            properties.string,
            properties.primary.get()
        );

        // Primary is at position (0, 0)
        if properties.primary.get() {
            primary_index = result.len();
        }

        result.push(properties);
    }

    Ok(DisplaySet {
        displays: result,
        primary_display: Cell::new(primary_index),
        paths: RefCell::new(paths),
        modes: RefCell::new(modes),
    })
}

/// Refreshes the screen to apply the changes
pub fn refresh() -> Result {
    // Re-query and re-apply current configuration
    let mut num_paths: u32 = 0;
    let mut num_modes: u32 = 0;

    unsafe {
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut num_paths, &mut num_modes)
            .ok()
            .map_err(|e| {
                DisplayError::WinAPI(format!("GetDisplayConfigBufferSizes failed: {:?}", e))
            })?;

        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); num_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); num_modes as usize];

        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut num_paths,
            paths.as_mut_ptr(),
            &mut num_modes,
            modes.as_mut_ptr(),
            None,
        )
        .ok()
        .map_err(|e| DisplayError::WinAPI(format!("QueryDisplayConfig failed: {:?}", e)))?;

        let result = SetDisplayConfig(
            Some(&paths[..num_paths as usize]),
            Some(&modes[..num_modes as usize]),
            SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES,
        );

        if result == 0 {
            // ERROR_SUCCESS
            Ok(())
        } else {
            Err(DisplayError::FailedToCommit(result))
        }?;
    }

    Ok(())
}

/// Returns the current display topology from the Windows topology database.
pub fn query_topology() -> Result<crate::Topology> {
    let mut num_paths: u32 = 0;
    let mut num_modes: u32 = 0;

    unsafe {
        GetDisplayConfigBufferSizes(QDC_DATABASE_CURRENT, &mut num_paths, &mut num_modes)
            .ok()
            .map_err(|e| {
                DisplayError::WinAPI(format!("GetDisplayConfigBufferSizes failed: {:?}", e))
            })?;
    }

    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); num_paths as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); num_modes as usize];
    let mut topology_id = DISPLAYCONFIG_TOPOLOGY_ID::default();

    unsafe {
        QueryDisplayConfig(
            QDC_DATABASE_CURRENT,
            &mut num_paths,
            paths.as_mut_ptr(),
            &mut num_modes,
            modes.as_mut_ptr(),
            Some(&mut topology_id as *mut DISPLAYCONFIG_TOPOLOGY_ID),
        )
        .ok()
        .map_err(|e| DisplayError::WinAPI(format!("QueryDisplayConfig failed: {:?}", e)))?;
    }

    Ok(match topology_id {
        DISPLAYCONFIG_TOPOLOGY_INTERNAL => crate::Topology::Internal,
        DISPLAYCONFIG_TOPOLOGY_CLONE    => crate::Topology::Clone,
        DISPLAYCONFIG_TOPOLOGY_EXTEND   => crate::Topology::Extend,
        DISPLAYCONFIG_TOPOLOGY_EXTERNAL => crate::Topology::External,
        DISPLAYCONFIG_TOPOLOGY_ID(v)    => crate::Topology::Unknown(v),
    })
}

/// Probes which topology modes Windows can switch to for the current display set.
/// Uses SetDisplayConfig with SDC_VALIDATE (dry-run) for each topology type.
/// Returns a list of (Topology, is_current) pairs.
pub fn query_stored_topologies() -> Result<Vec<(crate::Topology, bool)>> {
    let current = query_topology()?;
    let candidates = [
        (SDC_TOPOLOGY_INTERNAL, crate::Topology::Internal),
        (SDC_TOPOLOGY_CLONE,    crate::Topology::Clone),
        (SDC_TOPOLOGY_EXTEND,   crate::Topology::Extend),
        (SDC_TOPOLOGY_EXTERNAL, crate::Topology::External),
    ];
    let mut result = Vec::new();
    for (flag, topology) in candidates {
        let ret = unsafe { SetDisplayConfig(None, None, flag | SDC_VALIDATE) };
        if ret == 0 {
            let is_current = topology == current;
            result.push((topology, is_current));
        }
    }
    Ok(result)
}

/// A stored display configuration entry from the Windows Connectivity database.
/// Windows remembers one "recent" topology per physical display set and applies
/// it automatically when that set of monitors is detected.
pub struct ConnectivityEntry {
    /// Full registry key name including the hash suffix (`<SetId>^<Hash>`)
    pub key_name: String,
    /// The display set identifier (monitor IDs joined with `^`)
    pub set_id: String,
    /// The configuration key Windows will apply next time this display set connects
    pub recent: Option<String>,
    /// Stored configuration key for Internal (primary-only) topology
    pub internal: Option<String>,
    /// Stored configuration key for External (secondary-only) topology
    pub external: Option<String>,
    /// Stored configuration key for Extend topology
    pub extend: Option<String>,
    /// Stored configuration key for Clone topology
    pub clone: Option<String>,
    /// Windows FILETIME timestamp for each topology's last-saved configuration
    pub internal_timestamp: Option<u64>,
    pub external_timestamp: Option<u64>,
    pub extend_timestamp: Option<u64>,
    pub clone_timestamp: Option<u64>,
}

impl ConnectivityEntry {
    /// Returns the topology name that will be applied on next connect, if determinable.
    pub fn recent_topology(&self) -> Option<&str> {
        let recent = self.recent.as_deref()?;
        if self.internal.as_deref() == Some(recent) { return Some("Internal"); }
        if self.external.as_deref() == Some(recent) { return Some("External"); }
        if self.extend.as_deref() == Some(recent)   { return Some("Extend"); }
        if self.clone.as_deref()   == Some(recent)  { return Some("Clone"); }
        None
    }

    /// Returns names of all topology types that have stored configurations.
    pub fn available_topologies(&self) -> Vec<&str> {
        let mut v = Vec::new();
        if self.internal.is_some() { v.push("Internal"); }
        if self.external.is_some() { v.push("External"); }
        if self.extend.is_some()   { v.push("Extend"); }
        if self.clone.is_some()    { v.push("Clone"); }
        v
    }

    /// Extracts model-code prefixes for each monitor in this display set.
    /// E.g. "DEL430F6C19C34_34_07E8_46^SNY07CB..." → ["DEL430F", "SNY07CB"].
    pub fn monitor_prefixes(&self) -> Vec<&str> {
        self.set_id.split('^').filter_map(|id| {
            let end = id.find('_').unwrap_or(id.len());
            if end == 0 { None } else { Some(&id[..end]) }
        }).collect()
    }

    /// Returns true if at least one topology's config ID was found in the Configuration key.
    pub fn has_any_configuration_key(&self) -> bool {
        self.internal_timestamp.is_some()
            || self.external_timestamp.is_some()
            || self.extend_timestamp.is_some()
            || self.clone_timestamp.is_some()
    }
}

/// A Configuration registry entry with no matching Connectivity reference.
pub struct OrphanedConfigEntry {
    /// Full registry key name including the hash suffix (`<ConfigId>^<Hash>`)
    pub key_name: String,
    /// The configuration ID prefix (before the hash)
    pub config_id: String,
    /// Windows FILETIME timestamp
    pub timestamp: u64,
}

/// Reads all Configuration registry entries as config_id → (full_key_name, timestamp).
fn read_config_entries() -> HashMap<String, (String, u64)> {
    use winreg::RegKey;
    use winreg::enums::HKEY_LOCAL_MACHINE;

    let mut map = HashMap::new();
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let config_key = match hklm
        .open_subkey(r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers\Configuration")
    {
        Ok(k) => k,
        Err(_) => return map,
    };

    for key_result in config_key.enum_keys() {
        let key_name = match key_result {
            Ok(k) => k,
            Err(_) => continue,
        };
        // Key names are "<ConfigId>^<Hash>" — extract the ConfigId prefix
        let config_id = match key_name.find('^') {
            Some(pos) => key_name[..pos].to_string(),
            None => key_name.clone(),
        };
        let sub = match config_key.open_subkey(&key_name) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Ok(ts) = sub.get_value::<u64, _>("Timestamp") {
            map.insert(config_id, (key_name, ts));
        }
    }

    map
}

/// Reads all entries from the Windows display Connectivity database and cross-references
/// them against the Configuration key to find orphaned entries on either side.
/// Requires admin rights (HKLM key is protected).
pub fn read_connectivity_database() -> Result<(Vec<ConnectivityEntry>, Vec<OrphanedConfigEntry>)> {
    use winreg::RegKey;
    use winreg::enums::HKEY_LOCAL_MACHINE;

    let config_entries = read_config_entries();

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let connectivity = hklm
        .open_subkey(r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers\Connectivity")
        .map_err(|e| DisplayError::WinAPI(
            format!("Cannot open Connectivity key (run as administrator): {}", e)
        ))?;

    let mut entries = Vec::new();
    let mut referenced_config_ids = std::collections::HashSet::new();

    for key_result in connectivity.enum_keys() {
        let key_name = key_result
            .map_err(|e| DisplayError::WinAPI(e.to_string()))?;
        let sub = connectivity
            .open_subkey(&key_name)
            .map_err(|e| DisplayError::WinAPI(e.to_string()))?;

        let set_id: String = sub.get_value("SetId").unwrap_or_default();
        let internal: Option<String> = sub.get_value("Internal").ok();
        let external: Option<String> = sub.get_value("External").ok();
        let extend:   Option<String> = sub.get_value("eXtend").ok();
        let clone:    Option<String> = sub.get_value("Clone").ok();

        for id in [&internal, &external, &extend, &clone].iter().filter_map(|o| o.as_deref()) {
            referenced_config_ids.insert(id.to_string());
        }

        let internal_timestamp = internal.as_deref().and_then(|id| config_entries.get(id).map(|(_, ts)| *ts));
        let external_timestamp = external.as_deref().and_then(|id| config_entries.get(id).map(|(_, ts)| *ts));
        let extend_timestamp   = extend.as_deref().and_then(|id| config_entries.get(id).map(|(_, ts)| *ts));
        let clone_timestamp    = clone.as_deref().and_then(|id| config_entries.get(id).map(|(_, ts)| *ts));

        entries.push(ConnectivityEntry {
            key_name: key_name.clone(),
            set_id,
            recent: sub.get_value("Recent").ok(),
            internal,
            external,
            extend,
            clone,
            internal_timestamp,
            external_timestamp,
            extend_timestamp,
            clone_timestamp,
        });
    }

    let mut orphaned: Vec<OrphanedConfigEntry> = config_entries
        .into_iter()
        .filter(|(config_id, _)| !referenced_config_ids.contains(config_id))
        .map(|(config_id, (key_name, timestamp))| OrphanedConfigEntry { key_name, config_id, timestamp })
        .collect();
    orphaned.sort_by(|a, b| a.key_name.cmp(&b.key_name));

    Ok((entries, orphaned))
}
