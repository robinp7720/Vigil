use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use image::{ColorType, ImageFormat};
use serde::{Deserialize, Serialize};
use tracing::warn;
use zbus::blocking::Connection;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;

use crate::{
    Notification, NotificationAction, NotificationConfig, NotificationState, NotificationUrgency,
    StateUpdate, SystemUpdate,
};

const NOTIFICATION_PATH: &str = "/org/freedesktop/Notifications";
const NOTIFICATION_NAME: &str = "org.freedesktop.Notifications";
const STATE_VERSION: u8 = 1;
const MAX_IMAGE_DIMENSION: i32 = 2_048;
const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const DND_QUEUE_REPLAY_LIMIT: usize = 20;

#[derive(Debug)]
pub enum NotificationCommand {
    Invoke { id: u32, action_key: String },
    Dismiss { id: u32 },
    Clear,
    SetDnd { enabled: bool },
    SetHovered { id: u32, hovered: bool },
    MarkRead,
    Snapshot { reply: Sender<NotificationState> },
}

#[derive(Clone, Debug)]
pub struct NotificationControl {
    sender: Sender<NotificationCommand>,
}

impl NotificationControl {
    pub fn invoke(&self, id: u32, action_key: impl Into<String>) -> Result<()> {
        self.sender
            .send(NotificationCommand::Invoke {
                id,
                action_key: action_key.into(),
            })
            .context("notification server is unavailable")
    }

    pub fn dismiss(&self, id: u32) -> Result<()> {
        self.sender
            .send(NotificationCommand::Dismiss { id })
            .context("notification server is unavailable")
    }

    pub fn clear(&self) -> Result<()> {
        self.sender
            .send(NotificationCommand::Clear)
            .context("notification server is unavailable")
    }

    pub fn set_dnd(&self, enabled: bool) -> Result<()> {
        self.sender
            .send(NotificationCommand::SetDnd { enabled })
            .context("notification server is unavailable")
    }

    pub fn set_hovered(&self, id: u32, hovered: bool) -> Result<()> {
        self.sender
            .send(NotificationCommand::SetHovered { id, hovered })
            .context("notification server is unavailable")
    }

    pub fn mark_read(&self) -> Result<()> {
        self.sender
            .send(NotificationCommand::MarkRead)
            .context("notification server is unavailable")
    }

    pub fn snapshot(&self) -> Result<NotificationState> {
        let (reply, receiver) = mpsc::channel();
        self.sender
            .send(NotificationCommand::Snapshot { reply })
            .context("notification server is unavailable")?;
        receiver
            .recv_timeout(Duration::from_secs(1))
            .context("notification server did not answer")
    }
}

#[derive(Clone, Debug)]
struct NotificationRequest {
    app_name: String,
    replaces_id: u32,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: HashMap<String, OwnedValue>,
    expire_timeout: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedState {
    version: u8,
    next_id: u32,
    dnd: bool,
    unread_ids: BTreeSet<u32>,
    items: Vec<Notification>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            next_id: 1,
            dnd: false,
            unread_ids: BTreeSet::new(),
            items: Vec::new(),
        }
    }
}

#[derive(Debug)]
struct NotificationManager {
    config: NotificationConfig,
    state_path: PathBuf,
    cache_dir: PathBuf,
    persisted: PersistedState,
    active_ids: BTreeSet<u32>,
    visible_ids: VecDeque<u32>,
    waiting_ids: VecDeque<u32>,
    dnd_queue: VecDeque<u32>,
    timeout_seconds: HashMap<u32, Option<i64>>,
    hovered_since: HashMap<u32, i64>,
}

impl NotificationManager {
    fn load(config: NotificationConfig, now_epoch: i64) -> Result<Self> {
        let state_path = notification_state_path()?;
        let cache_dir = notification_cache_dir()?;
        ensure_private_dir(
            state_path
                .parent()
                .context("notification state path has no parent")?,
        )?;
        ensure_private_dir(&cache_dir)?;
        let persisted = match fs::read(&state_path) {
            Ok(bytes) => match serde_json::from_slice::<PersistedState>(&bytes) {
                Ok(mut value) if value.version == STATE_VERSION => {
                    for item in &mut value.items {
                        item.actionable = false;
                        item.expires_at = None;
                    }
                    value
                }
                Ok(_) => {
                    warn!("ignoring notification state with an unsupported version");
                    PersistedState::default()
                }
                Err(error) => {
                    warn!("ignoring corrupt notification state: {error}");
                    PersistedState::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => PersistedState::default(),
            Err(error) => return Err(error).context("failed to read notification state"),
        };
        let mut manager = Self {
            config,
            state_path,
            cache_dir,
            persisted,
            active_ids: BTreeSet::new(),
            visible_ids: VecDeque::new(),
            waiting_ids: VecDeque::new(),
            dnd_queue: VecDeque::new(),
            timeout_seconds: HashMap::new(),
            hovered_since: HashMap::new(),
        };
        manager.prune(now_epoch);
        manager.persist()?;
        Ok(manager)
    }

    #[cfg(test)]
    fn with_paths(
        config: NotificationConfig,
        state_path: PathBuf,
        cache_dir: PathBuf,
        now_epoch: i64,
    ) -> Result<Self> {
        ensure_private_dir(state_path.parent().context("state path has no parent")?)?;
        ensure_private_dir(&cache_dir)?;
        let mut manager = Self {
            config,
            state_path,
            cache_dir,
            persisted: PersistedState::default(),
            active_ids: BTreeSet::new(),
            visible_ids: VecDeque::new(),
            waiting_ids: VecDeque::new(),
            dnd_queue: VecDeque::new(),
            timeout_seconds: HashMap::new(),
            hovered_since: HashMap::new(),
        };
        manager.prune(now_epoch);
        Ok(manager)
    }

    fn notify(&mut self, request: NotificationRequest, now_epoch: i64) -> Result<(u32, bool)> {
        self.prune(now_epoch);
        let urgency = urgency_hint(&request.hints);
        let transient = bool_hint(&request.hints, "transient").unwrap_or(false);
        let resident = bool_hint(&request.hints, "resident").unwrap_or(false);
        let id = if request.replaces_id != 0
            && self
                .persisted
                .items
                .iter()
                .any(|item| item.id == request.replaces_id)
        {
            request.replaces_id
        } else {
            self.allocate_id()
        };
        let replacing = self.persisted.items.iter().any(|item| item.id == id);
        let image_path = self.resolve_image(id, &request.hints)?;
        let timeout_seconds = expiration_seconds(request.expire_timeout, urgency, &self.config);
        let actions = parse_actions(&request.actions);
        let notification = Notification {
            id,
            app_name: request.app_name,
            app_icon: (!request.app_icon.trim().is_empty()).then_some(request.app_icon),
            summary: request.summary,
            body: sanitize_body(&request.body),
            actions,
            urgency,
            image_path,
            desktop_entry: string_hint(&request.hints, "desktop-entry"),
            progress: progress_hint(&request.hints),
            received_at: now_epoch,
            expires_at: None,
            transient,
            resident,
            actionable: true,
            duplicate_count: 1,
        };

        if replacing {
            if let Some(existing) = self.persisted.items.iter_mut().find(|item| item.id == id) {
                *existing = notification;
            }
        } else {
            self.persisted.items.insert(0, notification);
            self.persisted.unread_ids.insert(id);
        }
        self.active_ids.insert(id);
        self.visible_ids.retain(|candidate| *candidate != id);
        self.waiting_ids.retain(|candidate| *candidate != id);
        self.dnd_queue.retain(|candidate| *candidate != id);
        self.timeout_seconds.insert(id, timeout_seconds);

        if self.persisted.dnd && urgency != NotificationUrgency::Critical {
            self.dnd_queue.push_back(id);
            while self.dnd_queue.len() > usize::from(self.config.dnd_queue_limit) {
                self.dnd_queue.pop_front();
            }
        } else {
            self.show(id, now_epoch);
        }
        self.collapse_history_duplicates();
        self.persist()?;
        Ok((id, !replacing))
    }

    fn allocate_id(&mut self) -> u32 {
        loop {
            let id = self.persisted.next_id.max(1);
            self.persisted.next_id = id.wrapping_add(1).max(1);
            if !self.persisted.items.iter().any(|item| item.id == id) {
                return id;
            }
        }
    }

    fn show(&mut self, id: u32, now_epoch: i64) {
        if self.visible_ids.len() >= usize::from(self.config.visible_limit) {
            let is_critical = self
                .persisted
                .items
                .iter()
                .find(|item| item.id == id)
                .is_some_and(|item| item.urgency == NotificationUrgency::Critical);
            if is_critical {
                if let Some(displaced) = self.visible_ids.pop_back() {
                    self.pause_expiration(displaced);
                    self.waiting_ids.push_front(displaced);
                }
            } else {
                self.waiting_ids.push_back(id);
                return;
            }
        }
        self.start_expiration(id, now_epoch);
        self.visible_ids.push_front(id);
    }

    fn start_expiration(&mut self, id: u32, now_epoch: i64) {
        let expires_at = self
            .timeout_seconds
            .get(&id)
            .copied()
            .flatten()
            .map(|seconds| now_epoch.saturating_add(seconds.max(1)));
        if let Some(item) = self.persisted.items.iter_mut().find(|item| item.id == id) {
            item.expires_at = expires_at;
        }
    }

    fn pause_expiration(&mut self, id: u32) {
        if let Some(item) = self.persisted.items.iter_mut().find(|item| item.id == id) {
            item.expires_at = None;
        }
    }

    fn promote_waiting(&mut self, now_epoch: i64) {
        while self.visible_ids.len() < usize::from(self.config.visible_limit) {
            let Some(id) = self.waiting_ids.pop_front() else {
                break;
            };
            if self.active_ids.contains(&id) {
                self.start_expiration(id, now_epoch);
                self.visible_ids.push_front(id);
            }
        }
    }

    fn set_dnd(&mut self, enabled: bool, now_epoch: i64) -> Result<()> {
        if self.persisted.dnd == enabled {
            return Ok(());
        }
        self.persisted.dnd = enabled;
        if !enabled {
            let queued = self
                .dnd_queue
                .drain(..)
                .rev()
                .take(DND_QUEUE_REPLAY_LIMIT)
                .collect::<Vec<_>>();
            for id in queued.into_iter().rev() {
                self.show(id, now_epoch);
            }
        }
        self.persist()
    }

    fn mark_read(&mut self) -> Result<()> {
        self.persisted.unread_ids.clear();
        self.persist()
    }

    fn set_hovered(&mut self, id: u32, hovered: bool, now_epoch: i64) {
        if hovered {
            if self.visible_ids.contains(&id) {
                self.hovered_since.entry(id).or_insert(now_epoch);
            }
            return;
        }
        let Some(started_at) = self.hovered_since.remove(&id) else {
            return;
        };
        let paused_seconds = now_epoch.saturating_sub(started_at);
        if let Some(item) = self.persisted.items.iter_mut().find(|item| item.id == id) {
            item.expires_at = item
                .expires_at
                .map(|expires| expires.saturating_add(paused_seconds));
        }
    }

    fn dismiss(&mut self, id: u32) -> Result<bool> {
        let was_active = self.active_ids.remove(&id);
        self.visible_ids.retain(|candidate| *candidate != id);
        self.waiting_ids.retain(|candidate| *candidate != id);
        self.dnd_queue.retain(|candidate| *candidate != id);
        self.timeout_seconds.remove(&id);
        self.hovered_since.remove(&id);
        self.persisted.unread_ids.remove(&id);
        let removed =
            if let Some(index) = self.persisted.items.iter().position(|item| item.id == id) {
                let item = self.persisted.items.remove(index);
                remove_cached_image(&self.cache_dir, item.image_path.as_deref());
                true
            } else {
                false
            };
        if removed {
            self.promote_waiting(current_epoch());
            self.persist()?;
        }
        Ok(was_active)
    }

    fn close_active(&mut self, id: u32) -> Result<bool> {
        if !self.active_ids.remove(&id) {
            return Ok(false);
        }
        self.visible_ids.retain(|candidate| *candidate != id);
        self.waiting_ids.retain(|candidate| *candidate != id);
        self.dnd_queue.retain(|candidate| *candidate != id);
        self.timeout_seconds.remove(&id);
        self.hovered_since.remove(&id);
        if let Some(item) = self.persisted.items.iter_mut().find(|item| item.id == id) {
            item.actionable = false;
            item.expires_at = None;
        }
        self.promote_waiting(current_epoch());
        self.persist()?;
        Ok(true)
    }

    fn clear(&mut self) -> Result<Vec<u32>> {
        let active = self.active_ids.iter().copied().collect::<Vec<_>>();
        for item in &self.persisted.items {
            remove_cached_image(&self.cache_dir, item.image_path.as_deref());
        }
        self.persisted.items.clear();
        self.persisted.unread_ids.clear();
        self.active_ids.clear();
        self.visible_ids.clear();
        self.waiting_ids.clear();
        self.dnd_queue.clear();
        self.timeout_seconds.clear();
        self.hovered_since.clear();
        self.persist()?;
        Ok(active)
    }

    fn expired(&mut self, now_epoch: i64) -> Result<Vec<u32>> {
        let expired = self
            .persisted
            .items
            .iter()
            .filter(|item| {
                self.active_ids.contains(&item.id)
                    && !self.hovered_since.contains_key(&item.id)
                    && item.expires_at.is_some_and(|expires| expires <= now_epoch)
            })
            .map(|item| item.id)
            .collect::<Vec<_>>();
        for id in &expired {
            self.close_active(*id)?;
        }
        Ok(expired)
    }

    fn invoke(&mut self, id: u32, action_key: &str) -> Result<bool> {
        let item = self
            .persisted
            .items
            .iter()
            .find(|item| item.id == id && item.actionable)
            .context("notification is no longer actionable")?;
        let valid =
            action_key == "default" || item.actions.iter().any(|action| action.key == action_key);
        if !valid {
            bail!("notification action is no longer available");
        }
        Ok(!item.resident)
    }

    fn snapshot(&self) -> NotificationState {
        NotificationState {
            items: self.persisted.items.clone(),
            visible_ids: self.visible_ids.iter().copied().collect(),
            unread_count: u16::try_from(self.persisted.unread_ids.len()).unwrap_or(u16::MAX),
            dnd: self.persisted.dnd,
        }
    }

    fn resolve_image(
        &self,
        id: u32,
        hints: &HashMap<String, OwnedValue>,
    ) -> Result<Option<PathBuf>> {
        for key in ["image-data", "image_data", "icon_data"] {
            let Some(value) = hints.get(key) else {
                continue;
            };
            let tuple =
                <(i32, i32, i32, bool, i32, i32, Vec<u8>) as TryFrom<OwnedValue>>::try_from(
                    value.clone(),
                )
                .map_err(|error| anyhow!("invalid {key} hint: {error}"))?;
            return self.cache_raw_image(id, tuple).map(Some);
        }
        for key in ["image-path", "image_path"] {
            if let Some(path) = string_hint(hints, key).and_then(local_path) {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    fn cache_raw_image(
        &self,
        id: u32,
        (width, height, rowstride, has_alpha, bits, channels, bytes): (
            i32,
            i32,
            i32,
            bool,
            i32,
            i32,
            Vec<u8>,
        ),
    ) -> Result<PathBuf> {
        if !(1..=MAX_IMAGE_DIMENSION).contains(&width)
            || !(1..=MAX_IMAGE_DIMENSION).contains(&height)
            || bits != 8
            || !matches!((has_alpha, channels), (false, 3) | (true, 4))
            || bytes.len() > MAX_IMAGE_BYTES
        {
            bail!("raw notification image exceeds supported dimensions or format");
        }
        let packed_stride = width
            .checked_mul(channels)
            .context("notification image stride overflow")?;
        if rowstride < packed_stride {
            bail!("raw notification image rowstride is too small");
        }
        let required = usize::try_from(
            rowstride
                .checked_mul(height)
                .context("notification image length overflow")?,
        )?;
        if bytes.len() < required {
            bail!("raw notification image data is truncated");
        }
        let mut packed =
            Vec::with_capacity(usize::try_from(width * height * channels).unwrap_or_default());
        let packed_stride = usize::try_from(packed_stride)?;
        let rowstride = usize::try_from(rowstride)?;
        for row in 0..usize::try_from(height)? {
            let start = row * rowstride;
            packed.extend_from_slice(&bytes[start..start + packed_stride]);
        }
        let path = self.cache_dir.join(format!("{id}.png"));
        image::save_buffer_with_format(
            &path,
            &packed,
            u32::try_from(width)?,
            u32::try_from(height)?,
            if has_alpha {
                ColorType::Rgba8
            } else {
                ColorType::Rgb8
            },
            ImageFormat::Png,
        )
        .context("failed to cache notification image")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(path)
    }

    fn collapse_history_duplicates(&mut self) {
        for item in &mut self.persisted.items {
            item.duplicate_count = 0;
        }
        let mut start = 0;
        while start < self.persisted.items.len() {
            let mut end = start + 1;
            while end < self.persisted.items.len()
                && self.persisted.items[start].app_name == self.persisted.items[end].app_name
                && self.persisted.items[start].summary == self.persisted.items[end].summary
                && self.persisted.items[start].body == self.persisted.items[end].body
            {
                end += 1;
            }
            self.persisted.items[start].duplicate_count =
                u16::try_from(end - start).unwrap_or(u16::MAX);
            start = end;
        }
    }

    fn prune(&mut self, now_epoch: i64) {
        let cutoff = now_epoch.saturating_sub(i64::from(self.config.history_days) * 86_400);
        let mut removed_images = Vec::new();
        self.persisted.items.retain(|item| {
            let keep = item.received_at >= cutoff;
            if !keep {
                removed_images.push(item.image_path.clone());
            }
            keep
        });
        while self.persisted.items.len() > usize::from(self.config.history_limit) {
            if let Some(item) = self.persisted.items.pop() {
                removed_images.push(item.image_path);
            }
        }
        let kept = self
            .persisted
            .items
            .iter()
            .map(|item| item.id)
            .collect::<BTreeSet<_>>();
        self.persisted.unread_ids.retain(|id| kept.contains(id));
        self.active_ids.retain(|id| kept.contains(id));
        self.visible_ids.retain(|id| kept.contains(id));
        self.waiting_ids.retain(|id| kept.contains(id));
        self.dnd_queue.retain(|id| kept.contains(id));
        self.timeout_seconds.retain(|id, _| kept.contains(id));
        self.hovered_since.retain(|id, _| kept.contains(id));
        for path in removed_images.into_iter().flatten() {
            remove_cached_image(&self.cache_dir, Some(&path));
        }
    }

    fn persist(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.persisted)?;
        let temporary = self.state_path.with_extension("json.tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .context("failed to open notification state temporary file")?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temporary, &self.state_path)?;
        Ok(())
    }
}

struct NotificationInterface {
    manager: Arc<Mutex<NotificationManager>>,
    state_sender: Sender<StateUpdate>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NotificationInterface {
    fn get_capabilities(&self) -> Vec<String> {
        let mut capabilities = vec![
            "actions".to_string(),
            "body".to_string(),
            "body-markup".to_string(),
            "body-hyperlinks".to_string(),
            "icon-static".to_string(),
            "persistence".to_string(),
        ];
        if command_exists("canberra-gtk-play") {
            capabilities.push("sound".to_string());
        }
        capabilities
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "Vigil".to_string(),
            "Robin Decker".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
            "1.3".to_string(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> zbus::fdo::Result<u32> {
        let request = NotificationRequest {
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions,
            hints,
            expire_timeout,
        };
        let now_epoch = current_epoch();
        let sound = sound_request(&request.hints);
        let suppress_sound = bool_hint(&request.hints, "suppress-sound").unwrap_or(false);
        let urgency = urgency_hint(&request.hints);
        let mut manager = self
            .manager
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("notification state is poisoned".to_string()))?;
        let sound_allowed = manager.config.sound
            && !suppress_sound
            && (!manager.persisted.dnd || urgency == NotificationUrgency::Critical);
        let (id, is_new) = manager
            .notify(request, now_epoch)
            .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
        publish(&self.state_sender, &manager);
        drop(manager);
        if is_new && sound_allowed {
            play_sound(sound);
        }
        Ok(id)
    }

    fn close_notification(
        &self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let mut manager = self
            .manager
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("notification state is poisoned".to_string()))?;
        let closed = manager
            .close_active(id)
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        if !closed {
            return Err(zbus::fdo::Error::InvalidArgs(
                "notification does not exist".to_string(),
            ));
        }
        publish(&self.state_sender, &manager);
        drop(manager);
        zbus::block_on(Self::notification_closed(&emitter, id, 3))
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))
    }

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

pub fn spawn_notification_server(
    config: NotificationConfig,
    state_sender: Sender<StateUpdate>,
    cancelled: Arc<AtomicBool>,
) -> Result<(NotificationControl, thread::JoinHandle<()>)> {
    let manager = Arc::new(Mutex::new(NotificationManager::load(
        config,
        current_epoch(),
    )?));
    let (command_sender, command_receiver) = mpsc::channel();
    let control = NotificationControl {
        sender: command_sender,
    };
    let manager_for_thread = manager.clone();
    let state_for_thread = state_sender.clone();
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    let handle = thread::spawn(move || {
        let result = run_notification_server(
            manager_for_thread,
            state_for_thread,
            command_receiver,
            cancelled,
            ready_sender,
        );
        if let Err(error) = result {
            warn!("notification server stopped: {error:#}");
        }
    });
    match ready_receiver.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(())) => Ok((control, handle)),
        Ok(Err(message)) => {
            let _ = handle.join();
            Err(anyhow!(message))
        }
        Err(error) => Err(error).context("notification server did not become ready"),
    }
}

fn run_notification_server(
    manager: Arc<Mutex<NotificationManager>>,
    state_sender: Sender<StateUpdate>,
    commands: Receiver<NotificationCommand>,
    cancelled: Arc<AtomicBool>,
    ready: mpsc::SyncSender<std::result::Result<(), String>>,
) -> Result<()> {
    let connection = match Connection::session()
        .context("failed to connect to the user D-Bus")
        .and_then(|connection| {
            connection
                .object_server()
                .at(
                    NOTIFICATION_PATH,
                    NotificationInterface {
                        manager: manager.clone(),
                        state_sender: state_sender.clone(),
                    },
                )
                .context("failed to export the notification interface")?;
            connection
                .request_name(NOTIFICATION_NAME)
                .context("failed to own org.freedesktop.Notifications")?;
            Ok(connection)
        }) {
        Ok(connection) => {
            let _ = ready.send(Ok(()));
            connection
        }
        Err(error) => {
            let _ = ready.send(Err(error.to_string()));
            return Err(error);
        }
    };
    {
        let manager = manager.lock().map_err(|_| anyhow!("state is poisoned"))?;
        publish(&state_sender, &manager);
    }

    while !cancelled.load(Ordering::Relaxed) {
        match commands.recv_timeout(Duration::from_millis(200)) {
            Ok(command) => handle_command(&connection, &manager, &state_sender, command)?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let expired = {
            let mut manager = manager.lock().map_err(|_| anyhow!("state is poisoned"))?;
            let expired = manager.expired(current_epoch())?;
            if !expired.is_empty() {
                publish(&state_sender, &manager);
            }
            expired
        };
        for id in expired {
            emit_closed(&connection, id, 1)?;
        }
    }
    Ok(())
}

fn handle_command(
    connection: &Connection,
    manager: &Arc<Mutex<NotificationManager>>,
    state_sender: &Sender<StateUpdate>,
    command: NotificationCommand,
) -> Result<()> {
    match command {
        NotificationCommand::Invoke { id, action_key } => {
            let close_after = {
                let mut manager = manager.lock().map_err(|_| anyhow!("state is poisoned"))?;
                manager.invoke(id, &action_key)?
            };
            emit_action(connection, id, &action_key)?;
            if close_after {
                let mut manager = manager.lock().map_err(|_| anyhow!("state is poisoned"))?;
                if manager.close_active(id)? {
                    publish(state_sender, &manager);
                    drop(manager);
                    emit_closed(connection, id, 2)?;
                }
            }
        }
        NotificationCommand::Dismiss { id } => {
            let mut manager = manager.lock().map_err(|_| anyhow!("state is poisoned"))?;
            let was_active = manager.dismiss(id)?;
            publish(state_sender, &manager);
            drop(manager);
            if was_active {
                emit_closed(connection, id, 2)?;
            }
        }
        NotificationCommand::Clear => {
            let mut manager = manager.lock().map_err(|_| anyhow!("state is poisoned"))?;
            let active = manager.clear()?;
            publish(state_sender, &manager);
            drop(manager);
            for id in active {
                emit_closed(connection, id, 2)?;
            }
        }
        NotificationCommand::SetDnd { enabled } => {
            let mut manager = manager.lock().map_err(|_| anyhow!("state is poisoned"))?;
            manager.set_dnd(enabled, current_epoch())?;
            publish(state_sender, &manager);
        }
        NotificationCommand::SetHovered { id, hovered } => {
            let mut manager = manager.lock().map_err(|_| anyhow!("state is poisoned"))?;
            manager.set_hovered(id, hovered, current_epoch());
        }
        NotificationCommand::MarkRead => {
            let mut manager = manager.lock().map_err(|_| anyhow!("state is poisoned"))?;
            manager.mark_read()?;
            publish(state_sender, &manager);
        }
        NotificationCommand::Snapshot { reply } => {
            let manager = manager.lock().map_err(|_| anyhow!("state is poisoned"))?;
            let _ = reply.send(manager.snapshot());
        }
    }
    Ok(())
}

fn emit_closed(connection: &Connection, id: u32, reason: u32) -> Result<()> {
    let interface = connection
        .object_server()
        .interface::<_, NotificationInterface>(NOTIFICATION_PATH)?;
    zbus::block_on(NotificationInterface::notification_closed(
        interface.signal_emitter(),
        id,
        reason,
    ))?;
    Ok(())
}

fn emit_action(connection: &Connection, id: u32, action_key: &str) -> Result<()> {
    let interface = connection
        .object_server()
        .interface::<_, NotificationInterface>(NOTIFICATION_PATH)?;
    zbus::block_on(NotificationInterface::action_invoked(
        interface.signal_emitter(),
        id,
        action_key,
    ))?;
    Ok(())
}

fn publish(sender: &Sender<StateUpdate>, manager: &NotificationManager) {
    let _ = sender.send(StateUpdate::System(SystemUpdate::Notifications(
        manager.snapshot(),
    )));
}

fn expiration_seconds(
    requested_ms: i32,
    urgency: NotificationUrgency,
    config: &NotificationConfig,
) -> Option<i64> {
    if urgency == NotificationUrgency::Critical || requested_ms == 0 {
        return None;
    }
    let seconds = if requested_ms > 0 {
        i64::from(requested_ms).saturating_add(999) / 1_000
    } else {
        match urgency {
            NotificationUrgency::Low => {
                i64::try_from(config.low_timeout_seconds).unwrap_or(i64::MAX)
            }
            NotificationUrgency::Normal => {
                i64::try_from(config.normal_timeout_seconds).unwrap_or(i64::MAX)
            }
            NotificationUrgency::Critical => return None,
        }
    };
    Some(seconds.max(1))
}

fn parse_actions(values: &[String]) -> Vec<NotificationAction> {
    values
        .chunks_exact(2)
        .filter(|pair| !pair[0].trim().is_empty())
        .map(|pair| NotificationAction {
            key: pair[0].clone(),
            label: pair[1].clone(),
        })
        .collect()
}

fn urgency_hint(hints: &HashMap<String, OwnedValue>) -> NotificationUrgency {
    match u8_hint(hints, "urgency") {
        Some(0) => NotificationUrgency::Low,
        Some(2..) => NotificationUrgency::Critical,
        _ => NotificationUrgency::Normal,
    }
}

fn progress_hint(hints: &HashMap<String, OwnedValue>) -> Option<u8> {
    i32_hint(hints, "value").map(|value| value.clamp(0, 100) as u8)
}

fn string_hint(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    <String as TryFrom<OwnedValue>>::try_from(hints.get(key)?.clone()).ok()
}

fn bool_hint(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    <bool as TryFrom<OwnedValue>>::try_from(hints.get(key)?.clone()).ok()
}

fn u8_hint(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
    <u8 as TryFrom<OwnedValue>>::try_from(hints.get(key)?.clone()).ok()
}

fn i32_hint(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<i32> {
    <i32 as TryFrom<OwnedValue>>::try_from(hints.get(key)?.clone()).ok()
}

#[derive(Clone, Debug)]
enum SoundRequest {
    File(PathBuf),
    Name(String),
}

fn sound_request(hints: &HashMap<String, OwnedValue>) -> Option<SoundRequest> {
    if let Some(path) = string_hint(hints, "sound-file").and_then(local_path)
        && path.is_absolute()
        && path.is_file()
    {
        return Some(SoundRequest::File(path));
    }
    string_hint(hints, "sound-name")
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 128
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
        })
        .map(SoundRequest::Name)
}

fn play_sound(sound: Option<SoundRequest>) {
    let Some(sound) = sound else {
        return;
    };
    if !command_exists("canberra-gtk-play") {
        return;
    }
    let result = match sound {
        SoundRequest::File(path) => Command::new("canberra-gtk-play")
            .arg("--file")
            .arg(path)
            .spawn(),
        SoundRequest::Name(name) => Command::new("canberra-gtk-play")
            .arg("--id")
            .arg(name)
            .spawn(),
    };
    if let Err(error) = result {
        warn!("failed to play notification sound: {error}");
    }
}

fn sanitize_body(body: &str) -> String {
    body.replace('\0', "").chars().take(16_384).collect()
}

fn local_path(value: String) -> Option<PathBuf> {
    let path = value
        .strip_prefix("file://")
        .map(percent_decode_path)
        .unwrap_or_else(|| PathBuf::from(value));
    path.is_absolute().then_some(path)
}

fn percent_decode_path(value: &str) -> PathBuf {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16)
        {
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    PathBuf::from(String::from_utf8_lossy(&decoded).into_owned())
}

fn notification_state_path() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(dirs::state_dir)
        .context("XDG state directory is unavailable")?;
    Ok(base.join("vigil").join("notifications.json"))
}

fn notification_cache_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(dirs::cache_dir)
        .context("XDG cache directory is unavailable")?;
    Ok(base.join("vigil").join("notifications"))
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn remove_cached_image(cache_dir: &Path, path: Option<&Path>) {
    let Some(path) = path else {
        return;
    };
    if path.parent() == Some(cache_dir) {
        let _ = fs::remove_file(path);
    }
}

fn command_exists(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(program).is_file())
    })
}

fn current_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn manager(now: i64) -> (tempfile::TempDir, NotificationManager) {
        let directory = tempfile::tempdir().unwrap();
        let manager = NotificationManager::with_paths(
            NotificationConfig::default(),
            directory.path().join("state").join("notifications.json"),
            directory.path().join("cache"),
            now,
        )
        .unwrap();
        (directory, manager)
    }

    fn request(summary: &str) -> NotificationRequest {
        NotificationRequest {
            app_name: "Test".to_string(),
            replaces_id: 0,
            app_icon: String::new(),
            summary: summary.to_string(),
            body: "Body".to_string(),
            actions: vec!["default".to_string(), "Open".to_string()],
            hints: HashMap::new(),
            expire_timeout: -1,
        }
    }

    #[test]
    fn ids_are_unique_and_replacements_do_not_increment_unread() {
        let (_directory, mut manager) = manager(1_000);
        let (first, _) = manager.notify(request("One"), 1_000).unwrap();
        let (second, _) = manager.notify(request("Two"), 1_001).unwrap();
        assert_ne!(first, second);
        assert_eq!(manager.snapshot().unread_count, 2);

        let mut replacement = request("Updated");
        replacement.replaces_id = first;
        let (id, is_new) = manager.notify(replacement, 1_002).unwrap();
        assert_eq!(id, first);
        assert!(!is_new);
        assert_eq!(manager.snapshot().unread_count, 2);
    }

    #[test]
    fn dnd_queues_normal_but_critical_bypasses() {
        let (_directory, mut manager) = manager(1_000);
        manager.set_dnd(true, 1_000).unwrap();
        let (normal, _) = manager.notify(request("Normal"), 1_000).unwrap();
        let mut critical = request("Critical");
        critical
            .hints
            .insert("urgency".to_string(), OwnedValue::from(2_u8));
        let (critical, _) = manager.notify(critical, 1_001).unwrap();
        assert!(!manager.snapshot().visible_ids.contains(&normal));
        assert!(manager.snapshot().visible_ids.contains(&critical));

        manager.set_dnd(false, 1_002).unwrap();
        assert!(manager.snapshot().visible_ids.contains(&normal));
    }

    #[test]
    fn critical_is_sticky_and_normal_uses_default_timeout() {
        let (_directory, mut manager) = manager(1_000);
        let (normal, _) = manager.notify(request("Normal"), 1_000).unwrap();
        assert_eq!(
            manager
                .persisted
                .items
                .iter()
                .find(|item| item.id == normal)
                .unwrap()
                .expires_at,
            Some(1_030)
        );
        let mut critical = request("Critical");
        critical
            .hints
            .insert("urgency".to_string(), OwnedValue::from(2_u8));
        let (critical, _) = manager.notify(critical, 1_001).unwrap();
        assert_eq!(
            manager
                .persisted
                .items
                .iter()
                .find(|item| item.id == critical)
                .unwrap()
                .expires_at,
            None
        );
    }

    #[test]
    fn overflow_waits_and_is_promoted_with_a_fresh_timeout() {
        let (_directory, mut manager) = manager(1_000);
        let first = manager.notify(request("One"), 1_000).unwrap().0;
        let second = manager.notify(request("Two"), 1_001).unwrap().0;
        let third = manager.notify(request("Three"), 1_002).unwrap().0;
        let fourth = manager.notify(request("Four"), 1_003).unwrap().0;

        assert_eq!(manager.snapshot().visible_ids.len(), 3);
        assert!(!manager.snapshot().visible_ids.contains(&fourth));
        assert_eq!(
            manager.waiting_ids.iter().copied().collect::<Vec<_>>(),
            vec![fourth]
        );

        manager.close_active(first).unwrap();
        assert!(manager.snapshot().visible_ids.contains(&fourth));
        let promoted_expiry = manager
            .persisted
            .items
            .iter()
            .find(|item| item.id == fourth)
            .unwrap()
            .expires_at
            .unwrap();
        assert!(promoted_expiry > current_epoch());
        assert!(promoted_expiry <= current_epoch().saturating_add(30));
        assert!(manager.snapshot().visible_ids.contains(&second));
        assert!(manager.snapshot().visible_ids.contains(&third));
    }

    #[test]
    fn exact_consecutive_duplicates_form_one_history_run() {
        let (_directory, mut manager) = manager(1_000);
        manager.notify(request("Same"), 1_000).unwrap();
        manager.notify(request("Same"), 1_001).unwrap();
        manager.notify(request("Same"), 1_002).unwrap();
        manager.notify(request("Different"), 1_003).unwrap();

        let items = &manager.snapshot().items;
        assert_eq!(items[0].duplicate_count, 1);
        assert_eq!(items[1].duplicate_count, 3);
        assert_eq!(items[2].duplicate_count, 0);
        assert_eq!(items[3].duplicate_count, 0);
    }

    #[test]
    fn hover_pauses_and_extends_the_toast_timeout() {
        let (_directory, mut manager) = manager(1_000);
        let id = manager.notify(request("Readable"), 1_000).unwrap().0;
        manager.set_hovered(id, true, 1_010);
        assert!(manager.expired(1_031).unwrap().is_empty());
        manager.set_hovered(id, false, 1_040);
        assert_eq!(
            manager
                .persisted
                .items
                .iter()
                .find(|item| item.id == id)
                .unwrap()
                .expires_at,
            Some(1_060)
        );
    }

    #[test]
    fn state_is_private_and_old_history_is_pruned() {
        let (directory, mut manager) = manager(1_000_000);
        let (id, _) = manager.notify(request("Old"), 1).unwrap();
        assert!(manager.persisted.items.iter().any(|item| item.id == id));
        manager.prune(1_000_000);
        manager.persist().unwrap();
        assert!(manager.persisted.items.is_empty());
        assert_eq!(
            fs::metadata(directory.path().join("state").join("notifications.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn malformed_action_tail_is_ignored() {
        let actions = parse_actions(&[
            "default".to_string(),
            "Open".to_string(),
            "orphan".to_string(),
        ]);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].key, "default");
    }

    #[test]
    fn sound_names_reject_shell_metacharacters() {
        let mut hints = HashMap::new();
        hints.insert(
            "sound-name".to_string(),
            OwnedValue::try_from(zbus::zvariant::Value::from("message;touch /tmp/no")).unwrap(),
        );
        assert!(sound_request(&hints).is_none());
    }
}
