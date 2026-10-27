//! Local filesystem capacity, sampled outside the GTK thread with a deadline.
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::SourceSupervisor;
use crate::{FilesystemUsage, SourceHealth, SourceId, StateUpdate, StorageState, SystemUpdate};

pub fn spawn_storage_source(
    sender: Sender<StateUpdate>,
    cancelled: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    SourceSupervisor::spawn(cancelled.clone(), Duration::from_secs(30), move || {
        match read_storage_state(&cancelled) {
            Ok(state) => {
                if sender
                    .send(StateUpdate::System(SystemUpdate::Storage(state)))
                    .is_err()
                {
                    cancelled.store(true, Ordering::Relaxed);
                }
                Ok(true)
            }
            Err(error) => {
                let _ = sender.send(StateUpdate::Health {
                    source: SourceId::Storage,
                    health: SourceHealth::Disconnected {
                        message: error.to_string(),
                    },
                });
                Err(error)
            }
        }
    })
}

fn read_storage_state(cancelled: &AtomicBool) -> Result<StorageState> {
    // GNU df omits duplicate devices/bind mounts unless -a is supplied. --local
    // avoids probing remote filesystems; the shared runner bounds slow devices.
    let output = crate::process::output(
        Command::new("df").env("LC_ALL", "C").args([
            "--local",
            "--block-size=1",
            "--output=fstype,size,used,avail,pcent,target",
        ]),
        crate::process::COMMAND_TIMEOUT,
        Some(cancelled),
    )?;
    if !output.status.success() {
        bail!(
            "filesystem usage unavailable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_df_output(std::str::from_utf8(&output.stdout).context("invalid filesystem usage text")?)
}

fn parse_df_output(output: &str) -> Result<StorageState> {
    let mut filesystems = Vec::new();
    let mut lines = output.lines();
    lines.next().context("missing filesystem usage header")?;
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let mut rest = line;
        let mut field = || -> Result<&str> {
            rest = rest.trim_start();
            if rest.is_empty() {
                bail!("incomplete filesystem usage row");
            }
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let value = &rest[..end];
            rest = &rest[end..];
            Ok(value)
        };
        let filesystem = field()?.to_string();
        let total = field()?.to_string();
        let used = field()?.to_string();
        let available = field()?.to_string();
        let percent = field()?.to_string();
        let mount_point = rest.trim_start();
        if is_temporary_filesystem(&filesystem) {
            continue;
        }
        let total_bytes = total
            .parse::<u64>()
            .context("invalid filesystem capacity")?;
        if total_bytes == 0 {
            continue;
        }
        if !mount_point.starts_with('/') {
            bail!("missing filesystem mount point");
        }
        filesystems.push(FilesystemUsage {
            mount_point: mount_point.to_string(),
            filesystem,
            total_bytes,
            used_bytes: used.parse().context("invalid used space")?,
            available_bytes: available
                .parse::<i128>()
                .context("invalid available space")?
                .clamp(0, u64::MAX as i128) as u64,
            used_percent: percent
                .trim_end_matches('%')
                .parse::<u16>()
                .context("invalid usage percentage")?
                .min(100) as u8,
        });
    }
    filesystems.sort_by(|a, b| {
        (a.mount_point != "/", &a.mount_point).cmp(&(b.mount_point != "/", &b.mount_point))
    });
    Ok(StorageState { filesystems })
}

fn is_temporary_filesystem(name: &str) -> bool {
    matches!(
        name,
        "tmpfs"
            | "devtmpfs"
            | "ramfs"
            | "efivarfs"
            | "squashfs"
            | "proc"
            | "sysfs"
            | "cgroup"
            | "cgroup2"
            | "hugetlbfs"
            | "debugfs"
            | "tracefs"
            | "securityfs"
            | "pstore"
            | "fusectl"
            | "configfs"
            | "mqueue"
            | "autofs"
            | "binfmt_misc"
            | "nsfs"
            | "rpc_pipefs"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_mount_paths_with_spaces_filters_virtual_filesystems_and_sorts_root_first() {
        let state = parse_df_output(
            "Type 1B-blocks Used Avail Use% Mounted on\n\
            tmpfs 100 10 90 10% /run\n\
            ext4 1000 850 100 90% /media/My disk\n\
            efivarfs 10 1 9 10% /sys/firmware/efi/efivars\n\
            btrfs 2000 500 1500 25% /\n",
        )
        .unwrap();
        assert_eq!(state.filesystems.len(), 2);
        assert_eq!(state.filesystems[0].mount_point, "/");
        let disk = &state.filesystems[1];
        assert_eq!(disk.mount_point, "/media/My disk");
        assert_eq!(disk.available_bytes, 100);
        // df's user-available percentage accounts for reserved blocks.
        assert_eq!(disk.used_percent, 90);
        assert_eq!(disk.total_bytes, 1000);
    }

    #[test]
    fn full_filesystems_clamp_negative_available_space_and_over_capacity_percentages() {
        let state = parse_df_output("header\next4 1000 1050 -100 111% /\n").unwrap();
        assert_eq!(state.filesystems[0].available_bytes, 0);
        assert_eq!(state.filesystems[0].used_percent, 100);
    }

    #[test]
    fn rejects_bad_or_incomplete_disk_data_without_publishing_a_false_empty_snapshot() {
        for output in [
            "",
            "header\next4 1000 20\n",
            "header\next4 unknown 20 980 2% /\n",
            "header\next4 1000 20 980 - /\n",
        ] {
            assert!(parse_df_output(output).is_err(), "{output:?}");
        }
        assert!(
            parse_df_output("header\ntmpfs 100 0 100 0% /run\n")
                .unwrap()
                .filesystems
                .is_empty()
        );
    }
}
