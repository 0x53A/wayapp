//! Focus-scoped native composition and main-thread keyboard repetition.
//! The worker only signals wakeups: XKB state, focused keys and event
//! generation never leave the application's thread.
//! Keycodes have already passed SCTK's raw-code-plus-eight XKB translation.
use crate::FrameScheduler;
use smithay_client_toolkit::seat::keyboard::KeyEvent;
use smithay_client_toolkit::seat::keyboard::Keymap;
use smithay_client_toolkit::seat::keyboard::Keysym;
use smithay_client_toolkit::seat::keyboard::RawModifiers;
use smithay_client_toolkit::seat::keyboard::RepeatInfo;
use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;
use wayland_backend::client::ObjectId;
use wayland_client::Proxy;
use wayland_client::protocol::wl_keyboard::WlKeyboard;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use xkbcommon::xkb;

struct HeldKey {
    raw_code: u32,
    pressed_at: Instant,
    deadline: Instant,
}

struct PressedKey {
    event: KeyEvent,
    base_utf8: Option<String>,
}

struct Keyboard {
    proxy: WlKeyboard,
    seat: ObjectId,
    info: RepeatInfo,
    state: Option<xkb::State>,
    compose: Option<xkb::compose::State>,
    modifiers: RawModifiers,
    layout: u32,
    pressed: HashMap<u32, PressedKey>,
    held: Option<HeldKey>,
    focus: Option<WlSurface>,
}

// SAFETY: Every XKB/Compose handle here belongs exclusively to this private
// keyboard record. Keymaps/tables are independently constructed (never borrowed
// from SCTK), and no C handle or clone escapes. All C calls require exclusive
// access through KeyboardRepeat's &mut methods. Moving the entire record
// between threads cannot race XKB's non-atomic refcounts. We deliberately do
// not add Sync.
unsafe impl Send for Keyboard {}

impl Drop for Keyboard {
    fn drop(&mut self) {
        if self.proxy.is_alive() {
            if self.proxy.version() >= 3 {
                self.proxy.release();
            } else if let Some(backend) = self.proxy.backend().upgrade() {
                // Legacy keyboards have no release request. Retire the
                // local proxy without sending an unsupported destructor.
                let _ = backend.destroy_object(&self.proxy.id());
            }
        }
    }
}

impl Keyboard {
    fn translate_repeat(&mut self, event: &mut KeyEvent) -> bool {
        if !self.focus.as_ref().is_some_and(Proxy::is_alive) {
            return false;
        }
        let Some(pressed) = self.pressed.get_mut(&event.raw_code) else {
            // Enter may announce an already-held key without a press callback.
            // Repeat it using the current mapping, never another focus's text.
            event.utf8 = self
                .state
                .as_ref()
                .map(|state| state.key_get_utf8(xkb::Keycode::new(event.raw_code + 8)));
            return true;
        };
        if let Some(state) = self.state.as_ref() {
            let raw = xkb::Keycode::new(event.raw_code + 8);
            let keysym = Keysym::new(state.key_get_one_sym(raw).raw());
            let base_utf8 = Some(state.key_get_utf8(raw));
            // Preserve composed text across redundant/unrelated modifier
            // updates. Only a change to this key's interpretation invalidates
            // its composed text, for the rest of this physical key press.
            if keysym != pressed.event.keysym || base_utf8 != pressed.base_utf8 {
                pressed.event.keysym = keysym;
                pressed.event.utf8 = base_utf8.clone();
                pressed.base_utf8 = base_utf8;
            }
            event.keysym = keysym;
        }
        event.utf8 = pressed.event.utf8.clone();
        true
    }
}

pub(crate) struct KeyboardRepeat {
    keyboards: HashMap<ObjectId, Keyboard>,
    scheduler: FrameScheduler,
}

impl KeyboardRepeat {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let mut scheduler = FrameScheduler::new(wake);
        scheduler.set_fps_target(1000.0);
        Self {
            keyboards: HashMap::new(),
            scheduler,
        }
    }

    pub fn add_keyboard(&mut self, seat: &WlSeat, keyboard: WlKeyboard) {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .find_map(|name| std::env::var_os(name).filter(|value| !value.is_empty()))
            .unwrap_or_else(|| "C".into());
        let compose =
            xkb::compose::Table::new_from_locale(&context, &locale, xkb::compose::COMPILE_NO_FLAGS)
                .ok()
                .map(|table| xkb::compose::State::new(&table, xkb::compose::STATE_NO_FLAGS));
        self.keyboards.insert(
            keyboard.id(),
            Keyboard {
                proxy: keyboard,
                seat: seat.id(),
                info: RepeatInfo::Disable,
                state: None,
                compose,
                modifiers: RawModifiers::default(),
                layout: 0,
                pressed: HashMap::new(),
                held: None,
                focus: None,
            },
        );
    }

    pub fn remove_keyboard(&mut self, seat: &WlSeat) -> Vec<WlSurface> {
        let mut focused = Vec::new();
        self.keyboards.retain(|_, keyboard| {
            if keyboard.seat != seat.id() {
                return true;
            }
            if let Some(surface) = &keyboard.focus {
                focused.push(surface.clone());
            }
            false
        });
        focused
    }

    pub fn contains_keyboard(&self, proxy: &WlKeyboard) -> bool {
        self.keyboards.contains_key(&proxy.id())
    }

    pub fn accepts_key_event(&self, proxy: &WlKeyboard) -> bool {
        proxy.is_alive()
            && (!self.contains_keyboard(proxy) || self.focused_surface(proxy).is_some())
    }

    pub fn focused_surface(&self, proxy: &WlKeyboard) -> Option<&WlSurface> {
        self.keyboards
            .get(&proxy.id())?
            .focus
            .as_ref()
            .filter(|surface| surface.is_alive())
    }

    pub fn focus_matches(&self, proxy: &WlKeyboard, surface: &WlSurface) -> bool {
        self.keyboards
            .get(&proxy.id())
            .and_then(|keyboard| keyboard.focus.as_ref())
            == Some(surface)
    }

    pub fn update_keymap(&mut self, proxy: &WlKeyboard, keymap: Keymap<'_>) {
        if let Some(keyboard) = self.keyboards.get_mut(&proxy.id()) {
            // A new mapping does not release logically pressed physical keys.
            // Keep their identity/text cache so release and both repeat modes
            // still work; translate_repeat refreshes meanings that changed.
            if let Some(compose) = keyboard.compose.as_mut() {
                compose.reset();
            }
            let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
            keyboard.state = xkb::Keymap::new_from_string(
                &context,
                keymap.as_string(),
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
            .map(|keymap| {
                let mut state = xkb::State::new(&keymap);
                state.update_mask(
                    keyboard.modifiers.depressed,
                    keyboard.modifiers.latched,
                    keyboard.modifiers.locked,
                    0,
                    0,
                    keyboard.layout,
                );
                state
            });
            if keyboard.state.is_none() {
                log::warn!("Unable to create keyboard repeat keymap");
            }
            if keyboard.held.as_ref().is_some_and(|held| {
                !keyboard.state.as_ref().is_some_and(|state| {
                    state
                        .get_keymap()
                        .key_repeats(xkb::Keycode::new(held.raw_code + 8))
                })
            }) {
                keyboard.held = None;
            }
        }
    }

    pub fn update_modifiers(&mut self, proxy: &WlKeyboard, modifiers: RawModifiers, layout: u32) {
        if let Some(keyboard) = self.keyboards.get_mut(&proxy.id()) {
            keyboard.modifiers = modifiers;
            keyboard.layout = layout;
            if let Some(state) = keyboard.state.as_mut() {
                state.update_mask(
                    modifiers.depressed,
                    modifiers.latched,
                    modifiers.locked,
                    0,
                    0,
                    layout,
                );
            }
        }
    }

    pub fn update_info(&mut self, proxy: &WlKeyboard, info: RepeatInfo) {
        if let Some(keyboard) = self.keyboards.get_mut(&proxy.id()) {
            keyboard.info = info;
            // A zero rate disables client repetition, including on compositors
            // that provide protocol-level repeated key events themselves.
            if matches!(info, RepeatInfo::Disable) {
                keyboard.held = None;
            }
        }
    }

    pub fn press(&mut self, proxy: &WlKeyboard, event: &mut KeyEvent) {
        if let Some(keyboard) = self.keyboards.get_mut(&proxy.id()) {
            if !keyboard.focus.as_ref().is_some_and(Proxy::is_alive) {
                return;
            }
            // SCTK's compose state is keyboard-global and survives leave/keymap
            // changes. Translate with our focus-scoped state instead, rather
            // than trying to infer stale composition from returned text.
            if let Some(compose) = keyboard.compose.as_mut() {
                event.utf8 = match compose.feed(xkb::Keysym::new(event.keysym.raw())) {
                    xkb::compose::FeedResult::Ignored => None,
                    xkb::compose::FeedResult::Accepted => match compose.status() {
                        xkb::compose::Status::Composed => {
                            let text = compose.utf8();
                            compose.reset();
                            text
                        }
                        xkb::compose::Status::Cancelled => {
                            compose.reset();
                            None
                        }
                        xkb::compose::Status::Composing => None,
                        xkb::compose::Status::Nothing => keyboard
                            .state
                            .as_ref()
                            .map(|state| state.key_get_utf8(xkb::Keycode::new(event.raw_code + 8))),
                    },
                };
            }
            let raw_code = event.raw_code + 8;
            let base_utf8 = keyboard
                .state
                .as_ref()
                .map(|state| state.key_get_utf8(xkb::Keycode::new(raw_code)));
            keyboard.pressed.insert(
                event.raw_code,
                PressedKey {
                    event: event.clone(),
                    base_utf8,
                },
            );
            let RepeatInfo::Repeat { delay, .. } = keyboard.info else {
                return;
            };
            if keyboard
                .state
                .as_ref()
                .is_some_and(|state| state.get_keymap().key_repeats(xkb::Keycode::new(raw_code)))
            {
                let now = Instant::now();
                keyboard.held = Some(HeldKey {
                    raw_code: event.raw_code,
                    pressed_at: now,
                    deadline: now + Duration::from_millis(u64::from(delay)),
                });
            }
        }
    }

    pub fn release(&mut self, proxy: &WlKeyboard, raw_code: u32) {
        if let Some(keyboard) = self.keyboards.get_mut(&proxy.id()) {
            keyboard.pressed.remove(&raw_code);
            if keyboard
                .held
                .as_ref()
                .is_some_and(|held| held.raw_code == raw_code)
            {
                keyboard.held = None;
            }
        }
    }

    pub fn server_repeat(&mut self, proxy: &WlKeyboard, event: &mut KeyEvent) -> bool {
        if !proxy.is_alive() {
            return false;
        }
        self.keyboards
            .get_mut(&proxy.id())
            .is_none_or(|keyboard| keyboard.translate_repeat(event))
    }

    pub fn leave(&mut self, proxy: &WlKeyboard) {
        if let Some(keyboard) = self.keyboards.get_mut(&proxy.id()) {
            keyboard.held = None;
            keyboard.pressed.clear();
            keyboard.focus = None;
        }
    }

    pub fn enter(&mut self, proxy: &WlKeyboard, surface: &WlSurface) {
        if let Some(keyboard) = self.keyboards.get_mut(&proxy.id()) {
            keyboard.held = None;
            keyboard.pressed.clear();
            keyboard.focus = Some(surface.clone());
            if let Some(compose) = keyboard.compose.as_mut() {
                compose.reset();
            }
        }
    }

    pub fn dispatch(&mut self) -> Vec<KeyEvent> {
        let now = Instant::now();
        let mut events = Vec::new();
        for keyboard in self.keyboards.values_mut() {
            // Destroying a focused wl_surface need not produce a leave event.
            if !keyboard.focus.as_ref().is_some_and(Proxy::is_alive) {
                keyboard.held = None;
                keyboard.pressed.clear();
                continue;
            }
            let Some(held) = keyboard.held.as_ref() else {
                continue;
            };
            if held.deadline > now {
                continue;
            }
            let RepeatInfo::Repeat { rate, .. } = keyboard.info else {
                continue;
            };
            // Press/release and focus changes keep held and its cache entry together.
            let mut event = keyboard.pressed[&held.raw_code].event.clone();
            event.time = event
                .time
                .wrapping_add(now.duration_since(held.pressed_at).as_millis() as u32);
            if !keyboard.translate_repeat(&mut event) {
                keyboard.held = None;
                continue;
            }
            events.push(event);
            // Do not replay a burst of missed repeats after a stalled UI.
            let gap = Duration::from_nanos(1_000_000_000 / u64::from(rate.get()))
                .max(Duration::from_millis(1));
            keyboard.held.as_mut().unwrap().deadline = now + gap;
        }
        // A stale earlier worker wakeup must not consume a newer deadline.
        if let Some(deadline) = self
            .keyboards
            .values()
            .filter_map(|keyboard| keyboard.held.as_ref().map(|held| held.deadline))
            .min()
        {
            self.scheduler
                .schedule_frame(deadline.saturating_duration_since(now));
        }
        events
    }
}
