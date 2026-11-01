//! The Rename action: dropping items on the wheel's Rename cell opens one
//! window listing them, each with the name the rules give it, updated on
//! every edit.
//!
//! Opening reads the items' metadata and their folders' names on a
//! short-lived worker thread (no disk work on the main thread), then the
//! window opens with the rules and order last used. The controller owns
//! the rules, the order and the preview; the window only draws. Rename runs
//! on the executor's file-job slot; on success the window closes and the
//! rules are remembered. A hidden window keeps no list: its models and
//! items are dropped, and the window itself after [`LazyWindow`]'s idle
//! time.

pub mod form;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use log::{debug, warn};
use slint::platform::Key;
use slint::winit_030::WinitWindowAccessor;
use slint::{
    CloseRequestResponse, ComponentHandle, LogicalPosition, LogicalSize, ModelRc, Timer, TimerMode,
    VecModel,
};
use vento_core::geometry::{Point, screen_for};
use vento_core::i18n::{fill, tr};
use vento_core::rename::{
    Batch, MAX_ITEMS, Order, OrderBy, Os, Preview, RealDisk, RenameMemory, RenameOutcome, Rule,
    RuleHistory, chip_summary, chip_title, footer_text, load_items, order_label, preview,
    read_taken, sort_items, steps, title_text, too_many_text, unknown_tags,
};
use vento_platform::Platform;

use crate::appearance;
use crate::exec::Executor;
use crate::lazy_window::LazyWindow;
use crate::panel::set_rows;
use crate::settings_hub::SettingsHub;
use crate::settings_window::{centred_on, show_title_bar};
use crate::toast::Toaster;
use crate::ui::{RenameChip, RenameRow, RenameWindow, Theme};
use form::{default_rule, form_to_rule, rows_of, rule_to_form};

/// The window's size when it opens (matches `rename.slint`), and the room
/// kept around it on a smaller screen.
const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 660.0;
const SCREEN_ROOM: f64 = 40.0;
/// How long a rename runs before the footer shows its progress, and how
/// often the progress is read after that.
const PROGRESS_AFTER: Duration = Duration::from_millis(500);
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

thread_local! {
    /// The controller, for items read on the opening thread. Set by
    /// [`RenameController::new`] on the main thread.
    static MAIN_RENAME: RefCell<Weak<RenameController>> = const { RefCell::new(Weak::new()) };
}

/// The open window's list and rules.
struct Shown {
    batch: Batch,
    rules: Vec<Rule>,
    order: Order,
    /// The Folders toggle: the rules rename folders too.
    include_dirs: bool,
    preview: Preview,
    /// Where the items were dropped, for the toasts.
    at: Point,
}

/// Opens, fills and closes the Rename window and runs its rename.
pub struct RenameController {
    platform: Rc<dyn Platform>,
    toaster: Rc<Toaster>,
    executor: Rc<Executor>,
    settings: Rc<SettingsHub>,
    window: LazyWindow<RenameWindow>,
    rows: Rc<VecModel<RenameRow>>,
    chips: Rc<VecModel<RenameChip>>,
    /// `Some` while the window is open.
    shown: RefCell<Option<Shown>>,
    history: RefCell<RuleHistory>,
    /// The rule whose popover edits are under way: its next edits join
    /// the history entry its first edit made.
    editing: Cell<Option<usize>>,
    /// A rename started from this list is running.
    busy: Cell<bool>,
    /// Counts drops; items read for an older one are dropped.
    loads: Cell<u64>,
    /// Counts lists shown (and hides); a rename or a re-read that finishes
    /// after its list was replaced or closed leaves the window alone.
    list: Cell<u64>,
    /// Shows a running rename's progress in the footer.
    progress: Timer,
    /// Whether another vento window is open (it keeps the focus when this
    /// one closes).
    other_window_open: RefCell<Option<Box<dyn Fn() -> bool>>>,
    this: Weak<Self>,
}

impl RenameController {
    /// No window yet.
    pub fn new(
        platform: Rc<dyn Platform>,
        toaster: Rc<Toaster>,
        executor: Rc<Executor>,
        settings: Rc<SettingsHub>,
    ) -> Rc<Self> {
        let controller = Rc::new_cyclic(|this| Self {
            platform,
            toaster,
            executor,
            settings,
            window: LazyWindow::new(),
            rows: Rc::new(VecModel::default()),
            chips: Rc::new(VecModel::default()),
            shown: RefCell::new(None),
            history: RefCell::new(RuleHistory::default()),
            editing: Cell::new(None),
            busy: Cell::new(false),
            loads: Cell::new(0),
            list: Cell::new(0),
            progress: Timer::default(),
            other_window_open: RefCell::new(None),
            this: this.clone(),
        });
        MAIN_RENAME.with(|slot| *slot.borrow_mut() = Rc::downgrade(&controller));
        // Chips, rows and footer are worded in Rust.
        appearance::follow_language(Rc::downgrade(&controller), |rename| rename.push());
        controller
    }

    /// Tells the controller how to ask whether another vento window is
    /// open.
    pub fn set_other_window_open(&self, f: Box<dyn Fn() -> bool>) {
        *self.other_window_open.borrow_mut() = Some(f);
    }

    /// Whether the window is open.
    pub fn is_open(&self) -> bool {
        self.shown.borrow().is_some()
    }

    /// Lists `paths`, dropped at `at` (global logical points), in the
    /// window. Over [`MAX_ITEMS`] only a toast says so. The items are read
    /// on a worker thread; an open window then takes the new list and keeps
    /// its rules.
    pub fn open(&self, paths: Vec<PathBuf>, at: Point) {
        if paths.len() > MAX_ITEMS {
            self.toaster.show(&too_many_text(), None, at);
            return;
        }
        let load = self.loads.get() + 1;
        self.loads.set(load);
        let spawned = std::thread::Builder::new()
            .name("vento-rename-open".into())
            .spawn(move || {
                let items = load_items(&paths);
                let taken = read_taken(&items, &RealDisk);
                let result = slint::invoke_from_event_loop(move || {
                    if let Some(controller) = MAIN_RENAME.with(|slot| slot.borrow().upgrade()) {
                        controller.loaded(load, Batch { items, taken }, at);
                    }
                });
                if let Err(e) = result {
                    warn!("cannot hand the items to the rename window: {e}");
                }
            });
        if let Err(e) = spawned {
            warn!("cannot start reading the items to rename: {e}");
            self.toaster.show(&tr("Couldn't read the items"), None, at);
        }
    }

    /// The items read, on the main thread: the list in the window, sorted
    /// and previewed.
    fn loaded(&self, load: u64, mut batch: Batch, at: Point) {
        if load != self.loads.get() {
            return;
        }
        if batch.items.is_empty() {
            self.toaster.show(&tr("Nothing to rename"), None, at);
            return;
        }
        let kept = self
            .shown
            .borrow_mut()
            .take()
            .map(|shown| (shown.rules, shown.order));
        let (rules, order) = kept.unwrap_or_else(|| {
            self.history.borrow_mut().clear();
            self.editing.set(None);
            let memory = self.settings.current().rename;
            (memory.rules, memory.order)
        });
        sort_items(&mut batch.items, order);
        let preview = preview(&batch, &rules, true, Os::HOST);
        *self.shown.borrow_mut() = Some(Shown {
            batch,
            rules,
            order,
            include_dirs: true,
            preview,
            at,
        });
        // A rename of the list this replaces goes on, but no longer holds
        // this one.
        self.list.set(self.list.get() + 1);
        self.set_busy(false);
        self.show(at);
    }

    /// Brings the window forward with the list, placed on the screen of
    /// `at` when it was not open yet.
    fn show(&self, at: Point) {
        let window = self.window.get_or_create(|| self.create());
        window.set_open_rule(-1);
        self.push();
        if !window.window().is_visible() {
            let screens = self.platform.screens();
            if let Some(screen) = screen_for(at, &screens) {
                let w = WIDTH.min(screen.w - SCREEN_ROOM).max(1.0);
                let h = HEIGHT.min(screen.h - SCREEN_ROOM).max(1.0);
                let (x, y) = centred_on(screen, w, h);
                window
                    .window()
                    .set_position(LogicalPosition::new(x as f32, y as f32));
                window
                    .window()
                    .set_size(LogicalSize::new(w as f32, h as f32));
            }
        }
        // vento runs as an accessory app: bring it forward so the window
        // comes to the front and takes the keyboard.
        self.platform.activate_app();
        if let Err(e) = window.show() {
            warn!("cannot show the rename window: {e}");
            self.hide();
            return;
        }
        let weak = window.as_weak();
        self.window.when_native(move || {
            let Some(window) = weak.upgrade() else { return };
            show_title_bar(&window);
            window.window().with_winit_window(|w| w.focus_window());
        });
        debug!("rename window open");
    }

    /// A new window, connected through weak references.
    fn create(&self) -> RenameWindow {
        let window = RenameWindow::new().expect("cannot create the rename window");
        window.set_rows(ModelRc::from(self.rows.clone()));
        window.set_chips(ModelRc::from(self.chips.clone()));
        // The Settings window's page rather than the panel's tint.
        window.global::<Theme>().set_clean(true);
        appearance::follow(&window, appearance::Look::Full);
        self.connect(&window);
        window
    }

    fn connect(&self, window: &RenameWindow) {
        let this = self.this.clone();
        window.on_tool_clicked(move |kind| {
            if let Some(rename) = this.upgrade() {
                rename.add_rule(kind);
            }
        });
        let this = self.this.clone();
        window.on_chip_clicked(move |i| {
            if let Some(rename) = this.upgrade() {
                rename.open_rule(i);
            }
        });
        let this = self.this.clone();
        window.on_chip_removed(move |i| {
            if let Some(rename) = this.upgrade() {
                rename.remove_rule(i);
            }
        });
        let this = self.this.clone();
        window.on_form_edited(move |form| {
            if let Some(rename) = this.upgrade() {
                rename.edit_rule(form_to_rule(&form));
            }
        });
        let this = self.this.clone();
        window.on_folders_toggled(move |on| {
            if let Some(rename) = this.upgrade() {
                rename.set_folders(on);
            }
        });
        let this = self.this.clone();
        window.on_order_chosen(move |by, reverse| {
            if let Some(rename) = this.upgrade() {
                rename.set_order(by, reverse);
            }
        });
        let this = self.this.clone();
        window.on_close_popover(move || {
            if let Some(rename) = this.upgrade() {
                rename.close_popover();
            }
        });
        let this = self.this.clone();
        window.on_rename_clicked(move || {
            if let Some(rename) = this.upgrade() {
                rename.rename();
            }
        });
        let this = self.this.clone();
        window.on_cancel_clicked(move || {
            if let Some(rename) = this.upgrade() {
                rename.hide();
            }
        });
        let this = self.this.clone();
        window.on_key_pressed(move |text, command| {
            this.upgrade()
                .is_some_and(|rename| rename.key(&text, command))
        });
        // The red button and ⌘W from the system.
        let this = self.this.clone();
        window.window().on_close_requested(move || {
            if let Some(rename) = this.upgrade() {
                rename.hide();
            }
            CloseRequestResponse::KeepWindowShown
        });
    }

    /// A key pressed in the window, with ⌘ (Ctrl): Esc closes the popover,
    /// then the window; ⌘↩ renames; ⌘Z undoes the last rule change; ⌘W
    /// closes. Returns whether it was used.
    fn key(&self, text: &str, command: bool) -> bool {
        let mut chars = text.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return false;
        };
        if c == char::from(Key::Escape) {
            if self.open_popover() != -1 {
                self.close_popover();
            } else {
                self.hide();
            }
            return true;
        }
        if !command {
            return false;
        }
        match c {
            c if c == char::from(Key::Return) => {
                self.rename();
                true
            }
            'z' | 'Z' => {
                self.undo();
                true
            }
            'w' | 'W' => {
                self.hide();
                true
            }
            _ => false,
        }
    }

    /// Which popover is open: -1 none, -2 Order, else a rule's index.
    fn open_popover(&self) -> i32 {
        self.window.get().map_or(-1, |w| w.get_open_rule())
    }

    fn close_popover(&self) {
        self.editing.set(None);
        if let Some(window) = self.window.get() {
            window.set_open_rule(-1);
        }
    }

    /// Opens rule `i`'s popover.
    fn open_rule(&self, i: i32) {
        if self.busy.get() {
            return;
        }
        let form = self.shown.borrow().as_ref().and_then(|shown| {
            let rule = shown.rules.get(usize::try_from(i).ok()?)?;
            Some(rule_to_form(rule))
        });
        self.editing.set(None);
        if let (Some(form), Some(window)) = (form, self.window.get()) {
            // The form first: the popover drawn for `open-rule` reads it.
            window.set_form(form);
            window.set_open_rule(i);
        }
    }

    /// A tool's rule, added at the end with its popover open.
    fn add_rule(&self, kind: i32) {
        let rule = default_rule(kind);
        let Some(i) = self.change_rules(None, |rules| {
            rules.push(rule.clone());
            Some(rules.len() - 1)
        }) else {
            return;
        };
        if let Some(window) = self.window.get() {
            window.set_form(rule_to_form(&rule));
            window.set_open_rule(i32::try_from(i).unwrap_or(-1));
        }
    }

    /// Rule `i` removed; its popover closes, a later rule's keeps its place.
    fn remove_rule(&self, i: i32) {
        let Ok(at) = usize::try_from(i) else { return };
        let removed = self.change_rules(None, |rules| {
            (at < rules.len()).then(|| {
                rules.remove(at);
            })
        });
        if removed.is_none() {
            return;
        }
        if let Some(window) = self.window.get() {
            let open = window.get_open_rule();
            if open == i {
                window.set_open_rule(-1);
            } else if open > i {
                window.set_open_rule(open - 1);
            }
        }
    }

    /// The open popover's rule, as its form now says.
    fn edit_rule(&self, rule: Rule) {
        let Ok(at) = usize::try_from(self.open_popover()) else {
            return;
        };
        self.change_rules(Some(at), |rules| {
            let slot = rules.get_mut(at)?;
            (*slot != rule).then(|| *slot = rule)
        });
    }

    /// Runs `change` on the rules; when it says it changed them (`Some`),
    /// the rules before go in the history (unless `edit`, the rule a
    /// popover edits, is already being edited) and the list is previewed
    /// again. Nothing changes while a rename runs.
    fn change_rules<R>(
        &self,
        edit: Option<usize>,
        change: impl FnOnce(&mut Vec<Rule>) -> Option<R>,
    ) -> Option<R> {
        if self.busy.get() {
            return None;
        }
        let result = {
            let mut shown = self.shown.borrow_mut();
            let shown = shown.as_mut()?;
            let before = shown.rules.clone();
            let result = change(&mut shown.rules)?;
            let mut editing = self.editing.get();
            record_change(&mut self.history.borrow_mut(), &mut editing, edit, before);
            self.editing.set(editing);
            result
        };
        self.refresh();
        Some(result)
    }

    /// Back to the rules before the last change, with no popover open.
    fn undo(&self) {
        if self.busy.get() {
            return;
        }
        self.editing.set(None);
        let Some(rules) = self.history.borrow_mut().undo() else {
            return;
        };
        if let Some(shown) = self.shown.borrow_mut().as_mut() {
            shown.rules = rules;
        }
        self.close_popover();
        self.refresh();
    }

    /// The Folders toggle.
    fn set_folders(&self, on: bool) {
        if self.busy.get() {
            return;
        }
        if let Some(shown) = self.shown.borrow_mut().as_mut() {
            shown.include_dirs = on;
        }
        self.refresh();
    }

    /// The Order popover's choice: the list sorted again and renumbered.
    fn set_order(&self, by: i32, reverse: bool) {
        if self.busy.get() {
            return;
        }
        let by = match by {
            1 => OrderBy::Modified,
            2 => OrderBy::Size,
            _ => OrderBy::Name,
        };
        if let Some(shown) = self.shown.borrow_mut().as_mut() {
            shown.order = Order { by, reverse };
            sort_items(&mut shown.batch.items, shown.order);
        }
        self.refresh();
    }

    /// Previews the list again and shows it.
    fn refresh(&self) {
        if let Some(shown) = self.shown.borrow_mut().as_mut() {
            shown.preview = preview(&shown.batch, &shown.rules, shown.include_dirs, Os::HOST);
        }
        self.push();
    }

    /// Gives the window the list, the chips, the title, the footer and the
    /// toolbar's state, as they are now.
    fn push(&self) {
        let Some(window) = self.window.get() else {
            return;
        };
        let shown = self.shown.borrow();
        let Some(shown) = shown.as_ref() else { return };
        let items = shown.batch.items.len();
        set_rows(&self.rows, rows_of(&shown.batch, &shown.preview, Os::HOST));
        set_rows(
            &self.chips,
            shown
                .rules
                .iter()
                .map(|rule| RenameChip {
                    title: chip_title(rule).into(),
                    summary: chip_summary(rule).into(),
                    open: false,
                    warn: warns(rule),
                })
                .collect::<Vec<_>>(),
        );
        window.set_title_text(title_text(items).into());
        let (before, red, after) = footer_text(&shown.preview, items);
        window.set_footer_before(before.into());
        window.set_footer_red(red.into());
        window.set_footer_after(after.into());
        window.set_can_rename(shown.preview.can_rename());
        window.set_has_folders(shown.batch.items.iter().any(|item| item.is_dir));
        window.set_include_folders(shown.include_dirs);
        window.set_order_label(order_label(shown.order.by).into());
        window.set_order_by(match shown.order.by {
            OrderBy::Name => 0,
            OrderBy::Modified => 1,
            OrderBy::Size => 2,
        });
        window.set_order_reverse(shown.order.reverse);
        window.set_busy(self.busy.get());
    }

    /// Renames the list as previewed, when nothing stands in the way. The
    /// window stays, held, until the executor says how it ended.
    fn rename(&self) {
        if self.busy.get() {
            return;
        }
        let job = self.shown.borrow().as_ref().and_then(|shown| {
            let steps = steps(&shown.batch, &shown.preview);
            (!steps.is_empty()).then(|| (steps, shown.rules.clone(), shown.order, shown.at))
        });
        let Some((steps, rules, order, at)) = job else {
            return;
        };
        let progress = Arc::new(AtomicUsize::new(0));
        if self.executor.is_busy() {
            // Another job runs: the executor toasts Busy and never answers,
            // so the window is not held.
            self.executor
                .rename_job(steps, Box::new(|_| {}), progress, at);
            return;
        }
        self.close_popover();
        self.set_busy(true);
        self.watch_progress(progress.clone(), steps.len());
        let list = self.list.get();
        let this = self.this.clone();
        // No borrow is held here: the executor may answer at once.
        self.executor.rename_job(
            steps,
            Box::new(move |outcome| {
                if let Some(rename) = this.upgrade() {
                    rename.finished(list, rules, order, outcome);
                }
            }),
            progress,
            at,
        );
    }

    /// Holds the window while a rename runs, or lets it go.
    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        if !busy {
            self.progress.stop();
        }
        if let Some(window) = self.window.get() {
            window.set_busy(busy);
            window.set_progress_text("".into());
        }
    }

    /// Shows in the footer how far the rename is, once it has run for
    /// [`PROGRESS_AFTER`]. `done` counts both phases' renames of `total`
    /// items.
    fn watch_progress(&self, done: Arc<AtomicUsize>, total: usize) {
        let started = Instant::now();
        let window = self.window.get().map(|w| w.as_weak());
        self.progress
            .start(TimerMode::Repeated, PROGRESS_EVERY, move || {
                if started.elapsed() < PROGRESS_AFTER {
                    return;
                }
                let Some(window) = window.as_ref().and_then(|w| w.upgrade()) else {
                    return;
                };
                let items = (done.load(Ordering::Relaxed) / 2).min(total);
                window.set_progress_text(progress_text(items, total).into());
            });
    }

    /// How rename `list` ended: on success the rules are remembered and the
    /// window closes; when the folder changed, its names are read again; on
    /// a failure the list stays as it was. A list replaced or closed since
    /// is left alone.
    fn finished(&self, list: u64, rules: Vec<Rule>, order: Order, outcome: &RenameOutcome) {
        if let RenameOutcome::Done(_) = outcome
            && let Err(e) = self
                .settings
                .update(|s| s.rename = RenameMemory { rules, order })
        {
            warn!("cannot remember the rename rules: {e}");
        }
        if list != self.list.get() {
            return;
        }
        self.set_busy(false);
        match outcome {
            RenameOutcome::Done(_) => self.hide(),
            RenameOutcome::Changed => self.reload(),
            RenameOutcome::Failed { .. } => self.push(),
        }
    }

    /// Reads the list's items and their folders' names again on a worker
    /// thread (an item renamed or deleted since drops out), then previews
    /// the list with them, in the order and with the rules it has then.
    fn reload(&self) {
        let Some(paths) = self.shown.borrow().as_ref().map(|shown| {
            shown
                .batch
                .items
                .iter()
                .map(|item| item.path.clone())
                .collect::<Vec<_>>()
        }) else {
            return;
        };
        let list = self.list.get();
        let spawned = std::thread::Builder::new()
            .name("vento-rename-reload".into())
            .spawn(move || {
                let items = load_items(&paths);
                let taken = read_taken(&items, &RealDisk);
                let result = slint::invoke_from_event_loop(move || {
                    if let Some(controller) = MAIN_RENAME.with(|slot| slot.borrow().upgrade()) {
                        controller.reloaded(list, Batch { items, taken });
                    }
                });
                if let Err(e) = result {
                    warn!("cannot hand the items to the rename window: {e}");
                }
            });
        if let Err(e) = spawned {
            warn!("cannot start reading the items again: {e}");
        }
    }

    /// The items read again for `list`; when none is left, the window
    /// closes.
    fn reloaded(&self, list: u64, mut batch: Batch) {
        if list != self.list.get() {
            return;
        }
        if batch.items.is_empty() {
            self.hide();
            return;
        }
        if let Some(shown) = self.shown.borrow_mut().as_mut() {
            sort_items(&mut batch.items, shown.order);
            shown.batch = batch;
        }
        self.refresh();
    }

    /// Hides the window and forgets its list and its rule history; the
    /// window itself goes later. Works while a rename runs too: it goes
    /// on and toasts its result. The focus goes back to the app that had
    /// it unless another vento window is open.
    fn hide(&self) {
        let Some(shown) = self.shown.borrow_mut().take() else {
            return;
        };
        drop(shown);
        self.list.set(self.list.get() + 1);
        self.set_busy(false);
        self.history.borrow_mut().clear();
        self.editing.set(None);
        self.rows.set_vec(Vec::new());
        self.chips.set_vec(Vec::new());
        if let Some(window) = self.window.get() {
            window.set_open_rule(-1);
            if let Err(e) = window.hide() {
                warn!("cannot hide the rename window: {e}");
            }
        }
        self.window.release_later();
        let this = self.this.clone();
        Timer::single_shot(Duration::ZERO, move || {
            let Some(rename) = this.upgrade() else { return };
            let other = rename
                .other_window_open
                .borrow()
                .as_ref()
                .is_some_and(|open| open());
            // Reopened in the meantime: vento keeps the focus.
            if !rename.is_open() && !other {
                rename.platform.deactivate_app();
            }
        });
        debug!("rename window closed");
    }
}

/// Puts `before` (the rules before a change) in the history, unless the
/// change is another edit of the rule already being edited, so a popover's
/// edits undo as one change. `edit` is the rule a popover edits, `None` for
/// an add or a removal; `editing` becomes it.
fn record_change(
    history: &mut RuleHistory,
    editing: &mut Option<usize>,
    edit: Option<usize>,
    before: Vec<Rule>,
) {
    if edit.is_none() || *editing != edit {
        history.record(before);
    }
    *editing = edit;
}

/// Whether a rule's chip warns: its template has a tag vento does not know.
fn warns(rule: &Rule) -> bool {
    matches!(rule, Rule::Template { text } if !unknown_tags(text).is_empty())
}

/// The footer while a rename runs.
fn progress_text(done: usize, total: usize) -> String {
    fill(
        &tr("Renaming… {done} of {total}"),
        &[("done", &done.to_string()), ("total", &total.to_string())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_popovers_edits_undo_as_one_change() {
        let r = |text: &str| Rule::Add {
            text: text.into(),
            after: false,
        };
        let mut history = RuleHistory::default();
        let mut editing = None;
        // Added, then three edits of rule 0.
        record_change(&mut history, &mut editing, None, vec![]);
        record_change(&mut history, &mut editing, Some(0), vec![r("")]);
        record_change(&mut history, &mut editing, Some(0), vec![r("a")]);
        record_change(&mut history, &mut editing, Some(0), vec![r("ab")]);
        assert_eq!(history.undo(), Some(vec![r("")]));
        assert_eq!(history.undo(), Some(vec![]));
        assert_eq!(history.undo(), None);

        // Rule 0 edited, rule 1's popover opened (the marker reset), edited.
        let mut editing = None;
        record_change(&mut history, &mut editing, Some(0), vec![r("x"), r("y")]);
        editing = None;
        record_change(&mut history, &mut editing, Some(1), vec![r("x0"), r("y")]);
        assert_eq!(history.undo(), Some(vec![r("x0"), r("y")]));
        assert_eq!(history.undo(), Some(vec![r("x"), r("y")]));
        assert_eq!(history.undo(), None);

        // Editing another rule without a reset starts a new entry too.
        let mut editing = Some(0);
        record_change(&mut history, &mut editing, Some(1), vec![r("z")]);
        assert_eq!(editing, Some(1));
        assert_eq!(history.undo(), Some(vec![r("z")]));
    }

    #[test]
    fn only_unknown_tags_warn() {
        assert!(!warns(&default_rule(0)));
        assert!(!warns(&Rule::Template {
            text: String::new()
        }));
        assert!(!warns(&Rule::new_number()));
        assert!(warns(&Rule::Template {
            text: "{nope}".into()
        }));
        assert!(!warns(&Rule::Template {
            text: "{name}".into()
        }));
    }

    #[test]
    fn progress_counts_items() {
        assert_eq!(progress_text(3, 9), "Renaming… 3 of 9");
    }
}
