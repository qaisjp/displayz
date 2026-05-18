use std::collections::HashMap;

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_TOPOLOGY_CLONE,
    DISPLAYCONFIG_TOPOLOGY_EXTEND, DISPLAYCONFIG_TOPOLOGY_EXTERNAL, DISPLAYCONFIG_TOPOLOGY_ID,
    DISPLAYCONFIG_TOPOLOGY_INTERNAL, GetDisplayConfigBufferSizes, QDC_DATABASE_CURRENT,
    QueryDisplayConfig, SDC_TOPOLOGY_CLONE, SDC_TOPOLOGY_EXTEND, SDC_TOPOLOGY_EXTERNAL,
    SDC_TOPOLOGY_INTERNAL, SDC_VALIDATE, SetDisplayConfig,
};

use crate::display::DisplayError;

type Result<T = ()> = std::result::Result<T, DisplayError>;

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
        if self.extend.as_deref()   == Some(recent) { return Some("Extend"); }
        if self.clone.as_deref()    == Some(recent) { return Some("Clone"); }
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

/// Sets the `Recent` value in a Connectivity entry to the config ID stored for `topology`.
/// Both `key_name` and the topology value must already exist.
pub fn set_connectivity_recent(key_name: &str, topology: &crate::Topology) -> Result {
    use winreg::RegKey;
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE};

    let value_name = topology
        .as_registry_value()
        .ok_or_else(|| DisplayError::WinAPI("Cannot set Recent to Unknown topology".to_string()))?;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let connectivity = hklm
        .open_subkey_with_flags(
            r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers\Connectivity",
            KEY_READ | KEY_WRITE,
        )
        .map_err(|e| DisplayError::WinAPI(format!("Cannot open Connectivity key (run as administrator): {}", e)))?;

    let sub = connectivity
        .open_subkey_with_flags(key_name, KEY_READ | KEY_WRITE)
        .map_err(|e| DisplayError::WinAPI(format!("Key '{}' not found: {}", key_name, e)))?;

    let config_id: String = sub
        .get_value(value_name)
        .map_err(|e| DisplayError::WinAPI(format!("Topology '{}' has no stored value in this entry: {}", topology, e)))?;

    sub.set_value("Recent", &config_id)
        .map_err(|e| DisplayError::WinAPI(format!("Failed to write Recent: {}", e)))?;

    Ok(())
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
