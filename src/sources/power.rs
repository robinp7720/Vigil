use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::{
    PowerProfile, PowerState, PowerStats, SourceHealth, SourceId, StateUpdate, SystemUpdate,
    ThresholdConfig,
};

use super::SourceSupervisor;

// Short enough for the Power page's live readings to feel current.
const POWER_POLL_INTERVAL: Duration = Duration::from_secs(5);
const POWER_SUPPLY_PATH: &str = "/sys/class/power_supply";
const CPU_SYSFS_PATH: &str = "/sys/devices/system/cpu";
const HWMON_PATH: &str = "/sys/class/hwmon";
const UPOWER_DESTINATION: &str = "org.freedesktop.UPower";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const UPOWER_INTERFACE: &str = "org.freedesktop.UPower";
const UPOWER_DEVICE_INTERFACE: &str = "org.freedesktop.UPower.Device";
const POWER_PROFILES_DESTINATION: &str = "net.hadess.PowerProfiles";
const POWER_PROFILES_PATH: &str = "/net/hadess/PowerProfiles";
const POWER_PROFILES_INTERFACE: &str = "net.hadess.PowerProfiles";
const DBUS_PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
/// hwmon drivers that report the CPU package, with the label to prefer.
const CPU_HWMON_SENSORS: [(&str, &[&str]); 4] = [
    ("k10temp", &["Tctl", "Tdie"]),
    ("zenpower", &["Tdie", "Tctl"]),
    ("coretemp", &["Package id 0"]),
    ("cpu_thermal", &[]),
];
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BatterySnapshot {
    present: bool,
    battery_percent: Option<u8>,
    charging: bool,
    time_to_empty_seconds: Option<u64>,
    time_to_full_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ProfileSnapshot {
    active: PowerProfile,
    available: Vec<PowerProfile>,
    degraded: Option<String>,
}

pub fn battery_severity(
    battery_percent: Option<u8>,
    charging: bool,
    thresholds: &ThresholdConfig,
) -> Option<&'static str> {
    if charging {
        return None;
    }

    let percent = battery_percent?;
    if percent <= thresholds.battery_critical_percent {
        Some("critical")
    } else if percent <= thresholds.battery_low_percent {
        Some("warning")
    } else {
        None
    }
}

pub fn spawn_power_source(
    sender: Sender<StateUpdate>,
    cancelled: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    spawn_profile_watcher(sender.clone(), cancelled.clone());
    let mut connection = None;
    SourceSupervisor::spawn(cancelled.clone(), POWER_POLL_INTERVAL, move || {
        if connection.is_none() {
            connection = Connection::system().ok();
        }
        match publish_power_snapshot(&sender, &cancelled, connection.as_ref()) {
            Ok(()) => Ok(true),
            Err(error) => {
                connection = None;
                let _ = sender.send(StateUpdate::Health {
                    source: SourceId::Power,
                    health: SourceHealth::Disconnected {
                        message: error.to_string(),
                    },
                });
                Err(error)
            }
        }
    })
}

/// Republishes as soon as power-profiles-daemon reports a change, so a profile
/// picked in the control center lights up without waiting for the next poll.
/// Detached: the signal iterator blocks, and the thread ends with the process
/// or on the first signal after the receiver is gone.
fn spawn_profile_watcher(sender: Sender<StateUpdate>, cancelled: Arc<AtomicBool>) {
    thread::spawn(move || {
        let Ok(connection) = Connection::system() else {
            return;
        };
        let Ok(proxy) = Proxy::new(
            &connection,
            POWER_PROFILES_DESTINATION,
            POWER_PROFILES_PATH,
            DBUS_PROPERTIES_INTERFACE,
        ) else {
            return;
        };
        let Ok(signals) = proxy.receive_signal("PropertiesChanged") else {
            return;
        };
        for _ in signals {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            let _ = publish_power_snapshot(&sender, &cancelled, Some(&connection));
        }
    });
}

fn publish_power_snapshot(
    sender: &Sender<StateUpdate>,
    cancelled: &Arc<AtomicBool>,
    connection: Option<&Connection>,
) -> Result<()> {
    let battery = read_power_battery(connection)?;
    let profiles = read_power_profiles(connection).unwrap_or_default();
    let mut stats = read_cpu_stats(Path::new(CPU_SYSFS_PATH), Path::new(HWMON_PATH));
    if battery.present {
        merge_battery_stats(
            &mut stats,
            &read_sysfs_battery_stats(Path::new(POWER_SUPPLY_PATH)),
            &battery,
        );
    }
    let update = StateUpdate::System(SystemUpdate::Power(PowerState {
        battery_present: battery.present,
        battery_percent: battery.battery_percent,
        charging: battery.charging,
        profile: profiles.active,
        available_profiles: profiles.available,
        performance_degraded: profiles.degraded,
        stats,
        changed_at: 0,
    }));

    if sender.send(update).is_err() {
        cancelled.store(true, Ordering::Relaxed);
    }

    Ok(())
}

fn read_power_battery(connection: Option<&Connection>) -> Result<BatterySnapshot> {
    match connection.map(read_upower_battery) {
        Some(Ok(snapshot)) => Ok(snapshot),
        _ => read_sysfs_battery(Path::new(POWER_SUPPLY_PATH)),
    }
}

fn read_upower_battery(connection: &Connection) -> Result<BatterySnapshot> {
    let upower = Proxy::new(
        connection,
        UPOWER_DESTINATION,
        UPOWER_PATH,
        UPOWER_INTERFACE,
    )
    .context("failed to build UPower proxy")?;
    let display_device: OwnedObjectPath = upower
        .call("GetDisplayDevice", &())
        .context("failed to query UPower display device")?;

    let device = Proxy::new(
        connection,
        UPOWER_DESTINATION,
        display_device.as_str(),
        UPOWER_DEVICE_INTERFACE,
    )
    .context("failed to build UPower display device proxy")?;

    let is_present: bool = device
        .get_property("IsPresent")
        .context("failed to read UPower IsPresent")?;
    if !is_present {
        return Ok(BatterySnapshot::default());
    }

    let percentage: f64 = device
        .get_property("Percentage")
        .context("failed to read UPower Percentage")?;
    let state: u32 = device
        .get_property("State")
        .context("failed to read UPower State")?;

    let time_to_empty: i64 = device.get_property("TimeToEmpty").unwrap_or_default();
    let time_to_full: i64 = device.get_property("TimeToFull").unwrap_or_default();

    Ok(BatterySnapshot {
        present: true,
        battery_percent: Some(clamp_percent_f64(percentage)),
        charging: matches!(state, 1 | 4 | 5),
        time_to_empty_seconds: positive_seconds(time_to_empty),
        time_to_full_seconds: positive_seconds(time_to_full),
    })
}

fn positive_seconds(value: i64) -> Option<u64> {
    u64::try_from(value).ok().filter(|seconds| *seconds > 0)
}

fn read_sysfs_battery(root: &Path) -> Result<BatterySnapshot> {
    let paths = battery_paths(root)?;
    if paths.is_empty() {
        return Ok(BatterySnapshot::default());
    }

    let battery_percent = weighted_capacity(&paths)?;
    let statuses = paths
        .iter()
        .map(|path| read_trimmed(path.join("status")))
        .collect::<Result<Vec<_>>>()?;
    let charging = statuses
        .iter()
        .any(|status| matches!(status.as_str(), "Charging" | "Full"));

    Ok(BatterySnapshot {
        present: true,
        battery_percent,
        charging,
        ..BatterySnapshot::default()
    })
}

/// Sums the raw sysfs counters of every battery. Figures are in milli-units
/// and stay `None` when no battery reports them.
fn read_sysfs_battery_stats(root: &Path) -> PowerStats {
    let mut stats = PowerStats::default();
    let Ok(paths) = battery_paths(root) else {
        return stats;
    };

    for path in &paths {
        let read = |name: &str| read_optional_u64(path.join(name)).ok().flatten();
        let voltage_uv = read("voltage_min_design").or_else(|| read("voltage_now"));
        // Batteries report either energy (µWh) or charge (µAh); convert charge
        // through the design voltage so both end up in mWh.
        let energy = |energy_name: &str, charge_name: &str| {
            read(energy_name)
                .or_else(|| {
                    read(charge_name)
                        .zip(voltage_uv)
                        .map(|(charge, volts)| charge.saturating_mul(volts) / 1_000_000)
                })
                .map(|micro| micro / 1_000)
        };
        add_stat(
            &mut stats.energy_now_mwh,
            energy("energy_now", "charge_now"),
        );
        add_stat(
            &mut stats.energy_full_mwh,
            energy("energy_full", "charge_full"),
        );
        add_stat(
            &mut stats.energy_full_design_mwh,
            energy("energy_full_design", "charge_full_design"),
        );
        let power_uw = read("power_now").or_else(|| {
            read("current_now")
                .zip(read("voltage_now"))
                .map(|(current, volts)| current.saturating_mul(volts) / 1_000_000)
        });
        add_stat(
            &mut stats.energy_rate_mw,
            power_uw.map(|micro| micro / 1_000),
        );
        if let Some(cycles) = read("cycle_count").filter(|cycles| *cycles > 0) {
            stats.cycle_count = Some(
                stats
                    .cycle_count
                    .unwrap_or_default()
                    .max(u32::try_from(cycles).unwrap_or(u32::MAX)),
            );
        }
        if let Some(limit) = read("charge_control_end_threshold").filter(|limit| *limit < 100) {
            stats.charge_limit_percent = Some(clamp_percent_u64(limit));
        }
    }

    stats
}

fn add_stat(total: &mut Option<u32>, value: Option<u64>) {
    if let Some(value) = value {
        let value = u32::try_from(value).unwrap_or(u32::MAX);
        *total = Some(total.unwrap_or_default().saturating_add(value));
    }
}

/// Folds battery readings into `stats`, preferring UPower's smoothed time
/// estimates and computing them from the raw rate when it has none.
fn merge_battery_stats(stats: &mut PowerStats, battery: &PowerStats, snapshot: &BatterySnapshot) {
    stats.energy_now_mwh = battery.energy_now_mwh;
    stats.energy_full_mwh = battery.energy_full_mwh;
    stats.energy_full_design_mwh = battery.energy_full_design_mwh;
    stats.cycle_count = battery.cycle_count;
    stats.charge_limit_percent = battery.charge_limit_percent;
    // Round to 0.1 W so tiny fluctuations do not redraw the bar every poll.
    stats.energy_rate_mw = battery
        .energy_rate_mw
        .filter(|rate| *rate > 0)
        .map(|rate| (rate + 50) / 100 * 100);

    let rate = u64::from(battery.energy_rate_mw.unwrap_or_default());
    let now = battery.energy_now_mwh.map(u64::from);
    let full = battery.energy_full_mwh.map(u64::from).map(|full| {
        let limit = u64::from(battery.charge_limit_percent.unwrap_or(100));
        full * limit / 100
    });
    if snapshot.charging {
        stats.time_to_full_seconds = snapshot.time_to_full_seconds.or_else(|| {
            now.zip(full)
                .filter(|(now, full)| rate > 0 && full > now)
                .map(|(now, full)| (full - now) * 3_600 / rate)
        });
    } else {
        stats.time_to_empty_seconds = snapshot
            .time_to_empty_seconds
            .or_else(|| now.filter(|_| rate > 0).map(|now| now * 3_600 / rate));
    }
}

fn read_cpu_stats(cpu_root: &Path, hwmon_root: &Path) -> PowerStats {
    let mut frequencies = Vec::new();
    let mut max_khz = None::<u64>;
    if let Ok(entries) = fs::read_dir(cpu_root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(index) = name.to_str().and_then(|name| name.strip_prefix("cpu")) else {
                continue;
            };
            if index.is_empty() || !index.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let cpufreq = entry.path().join("cpufreq");
            if let Ok(Some(current)) = read_optional_u64(cpufreq.join("scaling_cur_freq")) {
                frequencies.push(current);
            }
            if let Ok(Some(max)) = read_optional_u64(cpufreq.join("cpuinfo_max_freq")) {
                max_khz = Some(max_khz.unwrap_or_default().max(max));
            }
        }
    }

    let average_khz = (!frequencies.is_empty())
        .then(|| frequencies.iter().sum::<u64>() / u64::try_from(frequencies.len()).unwrap_or(1));
    let to_mhz = |khz: u64| u32::try_from((khz / 1_000 + 5) / 10 * 10).unwrap_or(u32::MAX);

    PowerStats {
        cpu_temp_celsius: read_cpu_temperature(hwmon_root),
        cpu_freq_mhz: average_khz.map(to_mhz),
        cpu_max_freq_mhz: max_khz.map(to_mhz),
        cpu_energy_preference: read_trimmed(
            cpu_root.join("cpu0/cpufreq/energy_performance_preference"),
        )
        .ok()
        .filter(|value| !value.is_empty()),
        ..PowerStats::default()
    }
}

fn read_cpu_temperature(hwmon_root: &Path) -> Option<i16> {
    let mut sensors = fs::read_dir(hwmon_root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = read_trimmed(path.join("name")).ok()?;
            let rank = CPU_HWMON_SENSORS
                .iter()
                .position(|(driver, _)| *driver == name)?;
            Some((rank, path))
        })
        .collect::<Vec<_>>();
    sensors.sort();
    let (rank, path) = sensors.into_iter().next()?;
    let labels = CPU_HWMON_SENSORS[rank].1;

    let mut inputs = fs::read_dir(&path)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let prefix = name.strip_suffix("_input")?.to_string();
            prefix.starts_with("temp").then_some(prefix)
        })
        .collect::<Vec<_>>();
    inputs.sort();
    let preferred = inputs.iter().find(|prefix| {
        read_trimmed(path.join(format!("{prefix}_label")))
            .is_ok_and(|label| labels.contains(&label.as_str()))
    });
    let prefix = preferred.or_else(|| inputs.first())?;
    let millidegrees = read_trimmed(path.join(format!("{prefix}_input")))
        .ok()?
        .parse::<i64>()
        .ok()?;
    i16::try_from((millidegrees + 500).div_euclid(1_000)).ok()
}

fn read_power_profiles(connection: Option<&Connection>) -> Result<ProfileSnapshot> {
    match connection.map(read_power_profiles_dbus) {
        Some(Ok(snapshot)) => Ok(snapshot),
        _ => read_power_profile_command().map(|active| ProfileSnapshot {
            active,
            ..ProfileSnapshot::default()
        }),
    }
}

fn read_power_profiles_dbus(connection: &Connection) -> Result<ProfileSnapshot> {
    let proxy = Proxy::new(
        connection,
        POWER_PROFILES_DESTINATION,
        POWER_PROFILES_PATH,
        POWER_PROFILES_INTERFACE,
    )
    .context("failed to build power profiles proxy")?;
    let profile: String = proxy
        .get_property("ActiveProfile")
        .context("failed to read power profile ActiveProfile")?;
    let profiles: Vec<HashMap<String, OwnedValue>> =
        proxy.get_property("Profiles").unwrap_or_default();
    let degraded: String = proxy
        .get_property("PerformanceDegraded")
        .unwrap_or_default();
    Ok(ProfileSnapshot {
        active: map_power_profile(&profile),
        available: available_profiles(&profiles),
        degraded: Some(degraded.trim().to_string()).filter(|reason| !reason.is_empty()),
    })
}

fn available_profiles(profiles: &[HashMap<String, OwnedValue>]) -> Vec<PowerProfile> {
    let mut available = profiles
        .iter()
        .filter_map(|profile| profile.get("Profile"))
        .filter_map(|value| String::try_from(value.try_clone().ok()?).ok())
        .map(|name| map_power_profile(&name))
        .collect::<Vec<_>>();
    available.dedup();
    available
}

fn read_power_profile_command() -> Result<PowerProfile> {
    let output = Command::new("powerprofilesctl")
        .arg("get")
        .output()
        .context("failed to execute powerprofilesctl get")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("powerprofilesctl get failed: {stderr}");
    }

    let stdout =
        String::from_utf8(output.stdout).context("powerprofilesctl output was not UTF-8")?;
    let profile = stdout.trim();
    if profile.is_empty() {
        bail!("powerprofilesctl get returned an empty profile");
    }

    Ok(map_power_profile(profile))
}

fn map_power_profile(value: &str) -> PowerProfile {
    match value.trim() {
        "performance" => PowerProfile::Performance,
        "power-saver" => PowerProfile::PowerSaver,
        _ => PowerProfile::Balanced,
    }
}

fn battery_paths(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut batteries = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if read_trimmed(path.join("type"))?.eq_ignore_ascii_case("battery") {
            batteries.push(path);
        }
    }
    batteries.sort();
    Ok(batteries)
}

fn weighted_capacity(paths: &[PathBuf]) -> Result<Option<u8>> {
    let mut current_total: u64 = 0;
    let mut full_total: u64 = 0;

    for path in paths {
        let current = read_optional_u64(path.join("energy_now"))?
            .or_else(|| read_optional_u64(path.join("charge_now")).ok().flatten());
        let full = read_optional_u64(path.join("energy_full"))?
            .or_else(|| read_optional_u64(path.join("charge_full")).ok().flatten());

        if let (Some(current), Some(full)) = (current, full)
            && full > 0
        {
            current_total = current_total.saturating_add(current);
            full_total = full_total.saturating_add(full);
        }
    }

    if full_total > 0 {
        let percent = ((current_total * 100) + (full_total / 2)) / full_total;
        return Ok(Some(clamp_percent_u64(percent)));
    }

    let mut capacities = Vec::new();
    for path in paths {
        if let Some(capacity) = read_optional_u64(path.join("capacity"))? {
            capacities.push(capacity);
        }
    }

    if capacities.is_empty() {
        return Ok(None);
    }

    let sum: u64 = capacities.iter().copied().sum();
    let percent = (sum + (u64::try_from(capacities.len()).unwrap_or(0) / 2))
        / u64::try_from(capacities.len()).unwrap_or(1);
    Ok(Some(clamp_percent_u64(percent)))
}

fn read_trimmed(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(text.trim().to_string())
}

fn read_optional_u64(path: impl AsRef<Path>) -> Result<Option<u64>> {
    let path = path.as_ref();
    match fs::read_to_string(path) {
        Ok(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                trimmed
                    .parse::<u64>()
                    .map(Some)
                    .with_context(|| format!("failed to parse integer from {}", path.display()))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(anyhow!(error)).with_context(|| format!("failed to read {}", path.display()))
        }
    }
}

fn clamp_percent_f64(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

fn clamp_percent_u64(value: u64) -> u8 {
    u8::try_from(value.min(100)).unwrap_or(100)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{PowerProfile, ThresholdConfig};

    use super::{
        BatterySnapshot, battery_severity, map_power_profile, merge_battery_stats, read_cpu_stats,
        read_sysfs_battery, read_sysfs_battery_stats,
    };

    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("vigil-power-{name}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn cpu_stats_average_clocks_and_prefer_the_package_sensor() {
        let root = scratch_dir("cpu");
        let cpu = root.join("cpu");
        for (index, khz) in [(0, 2_000_000), (1, 4_000_000)] {
            let cpufreq = cpu.join(format!("cpu{index}/cpufreq"));
            fs::create_dir_all(&cpufreq).unwrap();
            fs::write(cpufreq.join("scaling_cur_freq"), format!("{khz}\n")).unwrap();
            fs::write(cpufreq.join("cpuinfo_max_freq"), "4954565\n").unwrap();
        }
        fs::create_dir_all(cpu.join("cpufreq")).unwrap();
        fs::write(
            cpu.join("cpu0/cpufreq/energy_performance_preference"),
            "balance_performance\n",
        )
        .unwrap();
        let hwmon = root.join("hwmon");
        let nvme = hwmon.join("hwmon0");
        fs::create_dir_all(&nvme).unwrap();
        fs::write(nvme.join("name"), "nvme\n").unwrap();
        fs::write(nvme.join("temp1_input"), "41850\n").unwrap();
        let k10 = hwmon.join("hwmon1");
        fs::create_dir_all(&k10).unwrap();
        fs::write(k10.join("name"), "k10temp\n").unwrap();
        fs::write(k10.join("temp1_input"), "47375\n").unwrap();
        fs::write(k10.join("temp1_label"), "Tctl\n").unwrap();
        fs::write(k10.join("temp3_input"), "35250\n").unwrap();
        fs::write(k10.join("temp3_label"), "Tccd1\n").unwrap();

        let stats = read_cpu_stats(&cpu, &hwmon);

        assert_eq!(stats.cpu_freq_mhz, Some(3_000));
        assert_eq!(stats.cpu_max_freq_mhz, Some(4_950));
        assert_eq!(stats.cpu_temp_celsius, Some(47));
        assert_eq!(
            stats.cpu_energy_preference.as_deref(),
            Some("balance_performance")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn charge_based_batteries_convert_to_energy_and_estimate_runtime() {
        let root = scratch_dir("battery");
        let battery = root.join("BAT0");
        fs::create_dir(&battery).unwrap();
        for (name, value) in [
            ("type", "Battery"),
            ("status", "Discharging"),
            ("voltage_min_design", "11400000"),
            ("voltage_now", "12000000"),
            ("charge_now", "2000000"),
            ("charge_full", "4000000"),
            ("charge_full_design", "5000000"),
            ("current_now", "1000000"),
            ("cycle_count", "0"),
            ("charge_control_end_threshold", "80"),
        ] {
            fs::write(battery.join(name), format!("{value}\n")).unwrap();
        }

        let raw = read_sysfs_battery_stats(&root);
        assert_eq!(raw.energy_now_mwh, Some(22_800));
        assert_eq!(raw.energy_full_mwh, Some(45_600));
        assert_eq!(raw.energy_full_design_mwh, Some(57_000));
        assert_eq!(raw.energy_rate_mw, Some(12_000));
        assert_eq!(raw.cycle_count, None);
        assert_eq!(raw.charge_limit_percent, Some(80));

        let mut stats = crate::PowerStats::default();
        let snapshot = BatterySnapshot {
            present: true,
            ..BatterySnapshot::default()
        };
        merge_battery_stats(&mut stats, &raw, &snapshot);
        assert_eq!(stats.time_to_empty_seconds, Some(6_840));

        let charging = BatterySnapshot {
            charging: true,
            ..snapshot
        };
        let mut stats = crate::PowerStats::default();
        merge_battery_stats(&mut stats, &raw, &charging);
        // 80% of 45.6 Wh is 36.48 Wh, 13.68 Wh short at 12 W.
        assert_eq!(stats.time_to_full_seconds, Some(4_104));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn charging_battery_suppresses_warning_severity() {
        let thresholds = ThresholdConfig::default();

        assert_eq!(battery_severity(Some(6), true, &thresholds), None);
        assert_eq!(battery_severity(Some(15), true, &thresholds), None);
    }

    #[test]
    fn battery_severity_uses_exact_threshold_boundaries() {
        let thresholds = ThresholdConfig::default();

        assert_eq!(battery_severity(Some(16), false, &thresholds), None);
        assert_eq!(
            battery_severity(Some(15), false, &thresholds),
            Some("warning")
        );
        assert_eq!(
            battery_severity(Some(7), false, &thresholds),
            Some("critical")
        );
        assert_eq!(
            battery_severity(Some(6), false, &thresholds),
            Some("critical")
        );
    }

    #[test]
    fn power_profile_mapping_covers_expected_profiles() {
        assert_eq!(map_power_profile("performance"), PowerProfile::Performance);
        assert_eq!(map_power_profile("balanced"), PowerProfile::Balanced);
        assert_eq!(map_power_profile("power-saver"), PowerProfile::PowerSaver);
    }

    #[test]
    fn sysfs_battery_presence_distinguishes_desktops_and_laptops() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("cockpit-power-{unique}"));
        fs::create_dir_all(&root).unwrap();

        let desktop = read_sysfs_battery(&root).unwrap();
        assert!(!desktop.present);
        assert_eq!(desktop.battery_percent, None);

        let battery = root.join("BAT0");
        fs::create_dir(&battery).unwrap();
        fs::write(battery.join("type"), "Battery\n").unwrap();
        fs::write(battery.join("capacity"), "73\n").unwrap();
        fs::write(battery.join("status"), "Discharging\n").unwrap();
        let laptop = read_sysfs_battery(&root).unwrap();
        assert!(laptop.present);
        assert_eq!(laptop.battery_percent, Some(73));
        assert!(!laptop.charging);

        fs::remove_dir_all(root).unwrap();
    }
}
