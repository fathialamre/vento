//! What Vento has to say: a [`Notice`] is any message that used to be a toast,
//! and a [`JobNotice`] is a job that runs for a while. The [`Toaster`]
//! (crate::toast::Toaster) offers each to a [`NoticeSink`] (the virtual island)
//! and shows the classic toast and progress window when the sink does not take
//! it.

// The island sink and the call sites come in later steps.
#![allow(dead_code)]

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vento_core::actions::ActionId;
use vento_core::archive::Progress;
use vento_core::i18n::tr;
use vento_core::news::Tone;

/// The one button a notice can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonLabel {
    Undo,
    Cancel,
    Details,
    Resume,
    Change,
    GetIt,
    Restart,
    AddHour,
    Open,
}

impl ButtonLabel {
    /// The label in the language in use.
    pub fn text(self) -> String {
        match self {
            Self::Undo => tr("Undo"),
            Self::Cancel => tr("Cancel"),
            Self::Details => tr("Details"),
            Self::Resume => tr("Resume"),
            Self::Change => tr("Change"),
            Self::GetIt => tr("Get it"),
            Self::Restart => tr("Restart"),
            Self::AddHour => tr("+1 h"),
            Self::Open => tr("Open"),
        }
    }
}

/// A button on a notice and what it does when pressed.
pub struct NoticeButton {
    pub label: ButtonLabel,
    pub on: Rc<dyn Fn()>,
}

/// One message: what happened, how it reads, and what can be done about it.
pub struct Notice {
    pub tone: Tone,
    /// The action it is about, for the mark; none shows the tone's own.
    pub icon: Option<ActionId>,
    /// The sentence, as the classic toast shows it.
    pub text: String,
    /// Extra detail only the island shows; the classic toast never does.
    pub sub: String,
    pub button: Option<NoticeButton>,
    /// Not worth a toast: dropped when the island does not take it.
    pub island_only: bool,
    /// Stays until it has been hovered.
    pub sticky: bool,
    /// The executor's undo serial this notice offers to undo, if any.
    pub undo_serial: Option<u64>,
}

impl Notice {
    fn new(tone: Tone, text: impl Into<String>) -> Self {
        Self {
            tone,
            icon: None,
            text: text.into(),
            sub: String::new(),
            button: None,
            island_only: false,
            sticky: false,
            undo_serial: None,
        }
    }

    /// A finished job.
    pub fn done(text: impl Into<String>) -> Self {
        Self::new(Tone::Done, text)
    }

    /// Something to know.
    pub fn info(text: impl Into<String>) -> Self {
        Self::new(Tone::Info, text)
    }

    /// A failure; sticky, so it is not missed.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            sticky: true,
            ..Self::new(Tone::Error, text)
        }
    }

    /// Something still going on.
    pub fn busy(text: impl Into<String>) -> Self {
        Self::new(Tone::Busy, text)
    }

    pub fn with_icon(mut self, icon: ActionId) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn with_sub(mut self, sub: impl Into<String>) -> Self {
        self.sub = sub.into();
        self
    }

    pub fn with_button(mut self, label: ButtonLabel, on: impl Fn() + 'static) -> Self {
        self.button = Some(NoticeButton {
            label,
            on: Rc::new(on),
        });
        self
    }

    /// An Undo button running `undo`; none leaves the notice as it is.
    pub fn with_undo(mut self, undo: Option<Box<dyn Fn()>>) -> Self {
        if let Some(undo) = undo {
            self.button = Some(NoticeButton {
                label: ButtonLabel::Undo,
                on: Rc::from(undo),
            });
        }
        self
    }

    pub fn with_undo_serial(mut self, serial: u64) -> Self {
        self.undo_serial = Some(serial);
        self
    }

    pub fn island_only(mut self) -> Self {
        self.island_only = true;
        self
    }

    pub fn sticky(mut self) -> Self {
        self.sticky = true;
        self
    }
}

/// A job that runs for a while, for the island to show as work.
pub struct JobNotice {
    /// From [`next_job_id`]; ends with [`NoticeSink::job_ended`].
    pub id: u64,
    pub icon: ActionId,
    pub text: String,
    /// Where the job has got to; none while it cannot say.
    pub progress: Option<Arc<Progress>>,
    pub cancel: Option<Rc<dyn Fn()>>,
}

/// Where notices go before they become toasts.
pub trait NoticeSink {
    /// Takes the notice, or gives it back to be shown as a toast.
    fn notice(&self, n: Notice) -> Result<(), Notice>;
    /// Takes the job to show; `false` leaves it to the classic progress window.
    fn job_started(&self, job: JobNotice) -> bool;
    /// The job `id` is over, whether or not the sink took it.
    fn job_ended(&self, id: u64);
}

/// A job id no other job has had in this run.
pub fn next_job_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_is_sticky_and_the_others_are_not() {
        assert!(Notice::error("x").sticky);
        assert!(!Notice::done("x").sticky);
        assert!(!Notice::info("x").sticky);
        assert!(!Notice::busy("x").sticky);
    }

    #[test]
    fn with_undo_makes_an_undo_button_and_none_keeps_the_button() {
        let n = Notice::info("x").with_undo(Some(Box::new(|| {})));
        assert_eq!(n.button.as_ref().map(|b| b.label), Some(ButtonLabel::Undo));
        let n = Notice::info("x").with_undo(None);
        assert!(n.button.is_none());
        let n = Notice::info("x")
            .with_button(ButtonLabel::Open, || {})
            .with_undo(None);
        assert_eq!(n.button.as_ref().map(|b| b.label), Some(ButtonLabel::Open));
    }

    #[test]
    fn job_ids_differ() {
        assert_ne!(next_job_id(), next_job_id());
    }
}
