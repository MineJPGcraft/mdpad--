//! Crash-recovery drafts.
//!
//! The editor never writes to the user's file on its own — that is the whole
//! point of an explicit Save. But a process that dies (a panic, a power cut, a
//! forced reboot) would otherwise take an afternoon of typing with it, so the
//! document text is mirrored to a **separate** draft file in the settings
//! directory while it is dirty.
//!
//! The draft is never applied automatically. It is only offered, once, through a
//! single button in the status bar; if the user saves or discards, it is deleted.
//! That asymmetry is deliberate: silently restoring state across runs has burned
//! this project before, and the rule that came out of it is that persisted state
//! is only safe when the user can see it and refuse it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

thread_local! {
    /// Overrides where drafts are written; tests point this at their own file so
    /// a test run never touches the user's real draft (or reads a stale one).
    static DRAFT_PATH: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Points the draft file at `path` for this thread, returning the previous value.
pub fn set_draft_path(path: Option<PathBuf>) -> Option<PathBuf> {
    DRAFT_PATH.with(|p| std::mem::replace(&mut *p.borrow_mut(), path))
}

/// A document as it stood a few seconds before the process ended.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Draft {
    /// Where the document came from, if it had been saved before.
    pub path: Option<String>,
    /// The whole text at the time of writing.
    pub text: String,
    /// Unix seconds when the draft was written.
    pub saved_at: u64,
}

impl Draft {
    /// Where the draft lives.
    pub fn path_on_disk() -> Option<PathBuf> {
        if let Some(path) = DRAFT_PATH.with(|p| p.borrow().clone()) {
            return Some(path);
        }
        // A test that forgot to redirect must not write a draft made of test
        // documents into the developer's real settings directory, where the next
        // real launch would offer it back.
        if cfg!(test) {
            return None;
        }
        dirs::config_dir().map(|d| d.join("MarkdownPad").join("draft.json"))
    }

    /// Writes the draft, replacing any previous one.
    ///
    /// Written to a temporary file and then renamed: a crash halfway through a
    /// direct write would leave a truncated file that fails to parse, which is
    /// indistinguishable from having no draft at all.
    pub fn save(&self) -> anyhow::Result<()> {
        let Some(path) = Self::path_on_disk() else {
            anyhow::bail!("no config directory available");
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        // `rename` will not replace an existing file on Windows.
        let _ = std::fs::remove_file(&path);
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// The draft on disk, if there is a readable one.
    pub fn load() -> Option<Self> {
        let path = Self::path_on_disk()?;
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Deletes the draft and the temporary file, if either exists.
    pub fn discard() {
        let Some(path) = Self::path_on_disk() else {
            return;
        };
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("json.tmp"));
    }

    /// `true` when there is enough text to be worth offering back.
    ///
    /// A draft of an empty document is not a recovery, it is noise.
    pub fn is_worth_offering(&self) -> bool {
        !self.text.trim().is_empty()
    }

    /// How long ago the draft was written, for a human-readable label.
    pub fn age_words(&self, now: u64) -> String {
        relative_time(now.saturating_sub(self.saved_at))
    }
}

/// A rough, human-readable age: nobody needs seconds of precision here.
pub fn relative_time(age_secs: u64) -> String {
    match age_secs {
        0..=59 => "刚刚".to_owned(),
        60..=3599 => format!("{} 分钟前", age_secs / 60),
        3600..=86_399 => format!("{} 小时前", age_secs / 3600),
        _ => format!("{} 天前", age_secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "mdpad-draft-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    struct Scrubbed(Option<PathBuf>);

    impl Drop for Scrubbed {
        fn drop(&mut self) {
            let _ = set_draft_path(self.0.clone());
        }
    }

    fn with_temp_draft() -> (PathBuf, Scrubbed) {
        let path = temp_path();
        // Redirect *before* discarding, or the discard would delete whatever
        // draft the developer's own editor had left in the real config directory.
        let previous = set_draft_path(Some(path.clone()));
        Draft::discard();
        (path, Scrubbed(previous))
    }

    #[test]
    fn a_draft_round_trips_through_the_file() {
        let (path, _guard) = with_temp_draft();
        let draft = Draft {
            path: Some("/tmp/doc.md".to_owned()),
            text: "草稿内容\n".to_owned(),
            saved_at: 1_700_000_000,
        };
        draft.save().expect("write draft");
        assert!(path.is_file());
        let back = Draft::load().expect("read draft back");
        assert_eq!(back.path.as_deref(), Some("/tmp/doc.md"));
        assert_eq!(back.text, "草稿内容\n");
        assert_eq!(back.saved_at, 1_700_000_000);
    }

    #[test]
    fn saving_again_replaces_the_previous_draft() {
        let (_path, _guard) = with_temp_draft();
        Draft {
            path: None,
            text: "first".to_owned(),
            saved_at: 1,
        }
        .save()
        .expect("first write");
        Draft {
            path: None,
            text: "second".to_owned(),
            saved_at: 2,
        }
        .save()
        .expect("second write");
        assert_eq!(Draft::load().expect("load").text, "second");
        // The temporary file must not be left behind.
        assert!(Draft::path_on_disk().is_some_and(|p| !p.with_extension("json.tmp").exists()));
    }

    #[test]
    fn discarding_removes_the_draft() {
        let (path, _guard) = with_temp_draft();
        Draft {
            path: None,
            text: "x".to_owned(),
            saved_at: 0,
        }
        .save()
        .expect("write");
        Draft::discard();
        assert!(!path.exists());
        assert!(Draft::load().is_none());
    }

    #[test]
    fn a_corrupt_draft_is_ignored_instead_of_failing() {
        let (path, _guard) = with_temp_draft();
        std::fs::write(&path, "{ not json").expect("write garbage");
        assert!(Draft::load().is_none());
    }

    #[test]
    fn a_missing_draft_is_not_an_error() {
        let (path, _guard) = with_temp_draft();
        let _ = std::fs::remove_file(&path);
        assert!(Draft::load().is_none());
    }

    #[test]
    fn an_empty_draft_is_not_worth_offering() {
        let blank = Draft {
            path: None,
            text: "  \n\n".to_owned(),
            saved_at: 0,
        };
        assert!(!blank.is_worth_offering());
        let real = Draft {
            text: "字".to_owned(),
            ..blank
        };
        assert!(real.is_worth_offering());
    }

    #[test]
    fn relative_time_reads_naturally() {
        assert_eq!(relative_time(0), "刚刚");
        assert_eq!(relative_time(59), "刚刚");
        assert_eq!(relative_time(60), "1 分钟前");
        assert_eq!(relative_time(3599), "59 分钟前");
        assert_eq!(relative_time(3600), "1 小时前");
        assert_eq!(relative_time(86_400), "1 天前");
    }
}
