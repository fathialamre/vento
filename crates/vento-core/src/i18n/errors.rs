//! Wording for the reasons toasts give: Vento's own sentences for the common
//! `io::Error` kinds (Rust's text is always English), and the joining of the
//! clauses of one toast.

use std::io;

use super::tr;

/// Why `e` happened, as a short phrase.
///
/// The common kinds (not found, permission denied, already exists, disk
/// full, read-only disk, interrupted, invalid name) get Vento's own
/// translated wording. An error that carries a message of its own (Vento's
/// "cannot remove from read-only folder …") shows that message, and so do
/// the kinds without wording here: those fall back to Rust's `e.to_string()`,
/// which is English.
pub fn io_reason(e: &io::Error) -> String {
    use io::ErrorKind::*;
    // An operating system error and a bare kind carry no message of
    // their own, so the kind says it all.
    if e.get_ref().is_some() {
        return e.to_string();
    }
    match e.kind() {
        NotFound => tr("not found"),
        PermissionDenied => tr("permission denied"),
        AlreadyExists => tr("already exists"),
        StorageFull => tr("the disk is full"),
        ReadOnlyFilesystem => tr("the disk is read-only"),
        Interrupted => tr("interrupted"),
        InvalidFilename => tr("the name isn't valid"),
        _ => e.to_string(),
    }
}

/// The clauses of one toast, each a sentence of its own, joined with the
/// language's separator. Nothing is capitalised or changed: each clause
/// already starts the way it should.
pub fn clauses(parts: &[String]) -> String {
    parts.join(&tr("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::with_language;

    fn kind(k: io::ErrorKind) -> io::Error {
        io::Error::from(k)
    }

    #[test]
    fn common_kinds_read_in_english() {
        use io::ErrorKind::*;
        let say = |k| io_reason(&kind(k));
        assert_eq!(say(NotFound), "not found");
        assert_eq!(say(PermissionDenied), "permission denied");
        assert_eq!(say(AlreadyExists), "already exists");
        assert_eq!(say(StorageFull), "the disk is full");
        assert_eq!(say(ReadOnlyFilesystem), "the disk is read-only");
        assert_eq!(say(Interrupted), "interrupted");
        assert_eq!(say(InvalidFilename), "the name isn't valid");
    }

    #[test]
    fn common_kinds_read_in_arabic() {
        use io::ErrorKind::*;
        with_language("ar", || {
            let say = |k| io_reason(&kind(k));
            assert_eq!(say(NotFound), "غير موجود");
            assert_eq!(say(PermissionDenied), "لا توجد صلاحية");
            assert_eq!(say(AlreadyExists), "موجود مسبقاً");
            assert_eq!(say(StorageFull), "القرص ممتلئ");
            assert_eq!(say(ReadOnlyFilesystem), "القرص للقراءة فقط");
            assert_eq!(say(Interrupted), "توقّف قبل أن يكتمل");
            assert_eq!(say(InvalidFilename), "الاسم غير صالح");
        });
    }

    #[test]
    fn an_operating_system_error_is_worded_by_its_kind() {
        let missing = std::fs::File::open("/definitely/not/here/vento").unwrap_err();
        assert!(missing.raw_os_error().is_some());
        assert_eq!(io_reason(&missing), "not found");
        with_language("ar", || assert_eq!(io_reason(&missing), "غير موجود"));
    }

    #[test]
    fn other_kinds_and_messages_fall_back_to_the_text_of_the_error() {
        let other = io::Error::other("wrong format");
        assert_eq!(io_reason(&other), "wrong format");
        // The text of the error is English whatever the language.
        with_language("ar", || assert_eq!(io_reason(&other), "wrong format"));
        let plain = kind(io::ErrorKind::BrokenPipe);
        assert_eq!(io_reason(&plain), plain.to_string());
    }

    #[test]
    fn a_message_of_our_own_beats_the_kind() {
        let e = io::Error::new(io::ErrorKind::PermissionDenied, "cannot remove here");
        assert_eq!(io_reason(&e), "cannot remove here");
    }

    #[test]
    fn clauses_join_with_the_language_separator() {
        let parts = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(clauses(&parts), "a; b; c");
        assert_eq!(clauses(&parts[..1]), "a");
        assert_eq!(clauses(&[]), "");
        with_language("ar", || assert_eq!(clauses(&parts), "a؛ b؛ c"));
    }
}
