//! Writes the PDF action's pages with lopdf, where the system only reads
//! and draws PDFs (Windows.Data.Pdf). Each chosen page is copied with what
//! it uses (contents, fonts, images, annotations), each object once per
//! source; nothing else of a source comes along, not its outline, forms or
//! page tree, as PDFKit's copy on the Mac does.

use std::collections::{BTreeMap, HashMap};

use lopdf::{Dictionary, Document, Object, ObjectId};
use vento_core::pdf::{PageRef, PdfError, Source};

/// What a page takes from the page tree above it when it has none of its
/// own.
const INHERITED: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// How far up the page tree inherited attributes are looked for: a deeper
/// tree is damaged, or loops.
const MAX_TREE_DEPTH: usize = 64;

/// The PDF of `pages` (rotation applied), in order, with the document
/// information of `sources[pages[0].source]`.
pub(crate) fn assemble(sources: &[Source], pages: &[PageRef]) -> Result<Vec<u8>, PdfError> {
    let anchor = pages
        .first()
        .ok_or_else(|| PdfError::Backend("no pages to write".into()))?;
    let mut parts: Vec<Option<Part>> = (0..sources.len()).map(|_| None).collect();
    let mut out = Out::default();
    let tree = out.allocate();
    // Every page gets its id before any is copied, so a link from one
    // written page to another finds it whichever comes first. A page
    // written twice is linked to as its first copy.
    let mut written = Vec::with_capacity(pages.len());
    for &page in pages {
        let part = part(sources, &mut parts, page.source)?;
        let old = *part
            .pages
            .get(page.page)
            .ok_or_else(|| missing_page(page))?;
        let new = out.allocate();
        part.ids.copied.entry(old).or_insert(new);
        written.push((page, old, new));
    }
    for &(page, old, new) in &written {
        let part = parts[page.source].as_mut().expect("opened above");
        let mut dict = part.page_dict(old);
        let own = dict
            .get(b"Rotate")
            .ok()
            .and_then(|rotate| part.doc.dereference(rotate).ok())
            .and_then(|(_, rotate)| rotate.as_float().ok())
            .unwrap_or(0.0);
        dict.set("Rotate", i64::from(turned(own, page.turns)));
        let mut dict = part.ids.rewrite_dict(&part.doc, &dict, &mut out);
        dict.set("Parent", Object::Reference(tree));
        out.objects.insert(new, Object::Dictionary(dict));
    }
    let anchor = parts[anchor.source].as_mut().expect("opened above");
    let info = anchor
        .doc
        .trailer
        .get(b"Info")
        .ok()
        .map(|info| anchor.ids.rewrite(&anchor.doc, info, &mut out));
    for part in parts.iter_mut().flatten() {
        part.copy_pending(&mut out);
    }
    let version = parts
        .iter()
        .flatten()
        .map(|part| part.doc.version.as_str())
        .max()
        .unwrap_or("1.7")
        .to_owned();

    let kids = written
        .iter()
        .map(|&(_, _, new)| Object::Reference(new))
        .collect::<Vec<_>>();
    let mut root = Dictionary::new();
    root.set("Type", Object::Name(b"Pages".to_vec()));
    root.set("Count", kids.len() as i64);
    root.set("Kids", Object::Array(kids));
    out.objects.insert(tree, Object::Dictionary(root));
    let catalog = out.allocate();
    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"Catalog".to_vec()));
    dict.set("Pages", Object::Reference(tree));
    out.objects.insert(catalog, Object::Dictionary(dict));

    let mut doc = Document::with_version(version);
    doc.max_id = out.last;
    doc.objects = out.objects;
    doc.trailer.set("Root", Object::Reference(catalog));
    if let Some(info) = info.filter(|info| !info.is_null()) {
        doc.trailer.set("Info", info);
    }
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .map_err(|e| PdfError::Backend(format!("couldn't write the PDF: {e}")))?;
    Ok(bytes)
}

/// A page's own rotation in degrees plus `turns` quarter turns clockwise,
/// as 0, 90, 180 or 270. A rotation that is not a multiple of 90 (which
/// the PDF format forbids) is taken to the nearest one.
fn turned(own: f32, turns: u8) -> u32 {
    let quarters = (f64::from(own) / 90.0).round() as i64 + i64::from(turns);
    quarters.rem_euclid(4) as u32 * 90
}

fn missing_page(page: PageRef) -> PdfError {
    PdfError::Backend(format!(
        "no page {} in source {}",
        page.page + 1,
        page.source
    ))
}

/// Source `index` of `sources`, opened on first use and kept in `parts`.
fn part<'a>(
    sources: &[Source],
    parts: &'a mut [Option<Part>],
    index: usize,
) -> Result<&'a mut Part, PdfError> {
    let source = sources
        .get(index)
        .ok_or_else(|| PdfError::Backend(format!("no source {index}")))?;
    let slot = &mut parts[index];
    if slot.is_none() {
        *slot = Some(Part::open(source)?);
    }
    Ok(slot.as_mut().expect("just filled"))
}

/// The objects of the written PDF so far, and the last id given out.
#[derive(Default)]
struct Out {
    objects: BTreeMap<ObjectId, Object>,
    last: u32,
}

impl Out {
    /// A new object id, not yet used.
    fn allocate(&mut self) -> ObjectId {
        self.last += 1;
        (self.last, 0)
    }
}

/// One source, opened, with which of its objects have a copy.
struct Part {
    doc: Document,
    /// Its pages' ids, in page order.
    pages: Vec<ObjectId>,
    ids: Ids,
}

impl Part {
    /// `source` read by lopdf: `Locked` if it needs a password. A file that
    /// opens without one but restricts changes is decrypted, and written
    /// without that restriction, as PDFKit does.
    fn open(source: &Source) -> Result<Part, PdfError> {
        let doc = Document::load_mem(&source.bytes).map_err(|e| match e {
            lopdf::Error::InvalidPassword => PdfError::Locked,
            e => PdfError::Backend(format!("couldn't read the PDF: {e}")),
        })?;
        // Still encrypted: the empty password did not open it.
        if doc.is_encrypted() {
            return Err(PdfError::Locked);
        }
        let pages = doc.get_pages().into_values().collect();
        Ok(Part {
            doc,
            pages,
            ids: Ids::default(),
        })
    }

    /// Page `id`'s dictionary with what it inherits from the page tree
    /// filled in and no `Parent`; references still point into the source.
    fn page_dict(&self, id: ObjectId) -> Dictionary {
        let mut dict = self.doc.get_dictionary(id).cloned().unwrap_or_default();
        let mut parent = dict.get(b"Parent").and_then(Object::as_reference).ok();
        let mut depth = 0;
        while let Some(id) = parent
            && depth < MAX_TREE_DEPTH
            && INHERITED.iter().any(|key| !dict.has(key))
        {
            let Ok(node) = self.doc.get_dictionary(id) else {
                break;
            };
            for key in INHERITED {
                if !dict.has(key)
                    && let Ok(value) = node.get(key)
                {
                    dict.set(key.to_vec(), value.clone());
                }
            }
            parent = node.get(b"Parent").and_then(Object::as_reference).ok();
            depth += 1;
        }
        dict.remove(b"Parent");
        dict
    }

    /// Copies every object that copied objects reach, until none is left.
    fn copy_pending(&mut self, out: &mut Out) {
        while let Some((old, new)) = self.ids.pending.pop() {
            let copy = match self.doc.objects.get(&old) {
                Some(object) => self.ids.rewrite(&self.doc, object, out),
                None => Object::Null,
            };
            out.objects.insert(new, copy);
        }
    }
}

/// Which objects of one source have an id in the written PDF, and which of
/// those still wait to be copied.
#[derive(Default)]
struct Ids {
    copied: HashMap<ObjectId, ObjectId>,
    pending: Vec<(ObjectId, ObjectId)>,
}

impl Ids {
    /// `object` with every reference turned into one into the written PDF.
    /// What a reference leads to is copied later ([`Part::copy_pending`]),
    /// so a long chain of objects never deepens the stack.
    fn rewrite(&mut self, doc: &Document, object: &Object, out: &mut Out) -> Object {
        match object {
            Object::Reference(id) => self.reference(doc, *id, out),
            Object::Array(items) => Object::Array(
                items
                    .iter()
                    .map(|item| self.rewrite(doc, item, out))
                    .collect(),
            ),
            Object::Dictionary(dict) => Object::Dictionary(self.rewrite_dict(doc, dict, out)),
            Object::Stream(stream) => {
                let mut copy = stream.clone();
                copy.dict = self.rewrite_dict(doc, &stream.dict, out);
                Object::Stream(copy)
            }
            other => other.clone(),
        }
    }

    fn rewrite_dict(&mut self, doc: &Document, dict: &Dictionary, out: &mut Out) -> Dictionary {
        let mut copy = Dictionary::new();
        for (key, value) in dict.iter() {
            copy.set(key.clone(), self.rewrite(doc, value, out));
        }
        copy
    }

    /// The written PDF's reference for source object `id`. A page that is
    /// not written, and the source's page tree, become null: a link to a
    /// left-out page goes nowhere rather than bring the page along.
    fn reference(&mut self, doc: &Document, id: ObjectId, out: &mut Out) -> Object {
        if let Some(&new) = self.copied.get(&id) {
            return Object::Reference(new);
        }
        let Some(target) = doc.objects.get(&id) else {
            return Object::Null;
        };
        if is_page_tree(target) {
            return Object::Null;
        }
        let new = out.allocate();
        self.copied.insert(id, new);
        self.pending.push((id, new));
        Object::Reference(new)
    }
}

/// Whether `object` is a page or a node of the page tree.
fn is_page_tree(object: &Object) -> bool {
    matches!(object.type_name(), Ok(b"Page" | b"Pages"))
}

/// Builds test PDFs with lopdf, so no binary fixtures are kept. The same
/// pages as the Mac's fixtures: a red block in the top-left quarter of
/// each, away from the corners.
#[cfg(test)]
pub(crate) mod test_support {
    use lopdf::{Dictionary, Document, EncryptionState, EncryptionVersion, Object, Permissions};

    /// A PDF of `pages`, each `(width, height, rotate)` in points and
    /// degrees, locked with `password` when given.
    pub(crate) fn fixture(pages: &[(f64, f64, i64)], password: Option<&str>) -> Vec<u8> {
        build(pages, password, password, None, None)
    }

    /// An unlocked [`fixture`] whose document title is `title`.
    pub(crate) fn titled(pages: &[(f64, f64, i64)], title: &str) -> Vec<u8> {
        build(pages, None, None, Some(title), None)
    }

    /// A [`fixture`] that opens without a password but restricts changes
    /// with `owner`.
    pub(crate) fn owner_locked(pages: &[(f64, f64, i64)], owner: &str) -> Vec<u8> {
        build(pages, Some(""), Some(owner), None, None)
    }

    /// A one-page [`fixture`] whose crop box is `crop` (x, y, width, height
    /// in page coordinates) rather than the whole page.
    pub(crate) fn cropped(page: (f64, f64, i64), crop: [f64; 4]) -> Vec<u8> {
        build(&[page], None, None, None, Some(crop))
    }

    fn build(
        pages: &[(f64, f64, i64)],
        user: Option<&str>,
        owner: Option<&str>,
        title: Option<&str>,
        crop: Option<[f64; 4]>,
    ) -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let tree = doc.new_object_id();
        let mut kids = Vec::new();
        for &(w, h, rotate) in pages {
            let content = format!(
                "1 0 0 rg {} {} {} {} re f",
                w * 0.1,
                h * 0.6,
                w * 0.3,
                h * 0.3
            );
            let contents =
                doc.add_object(lopdf::Stream::new(Dictionary::new(), content.into_bytes()));
            let mut page = Dictionary::new();
            page.set("Type", Object::Name(b"Page".to_vec()));
            page.set("Parent", tree);
            page.set("MediaBox", rect([0.0, 0.0, w, h]));
            page.set("Contents", contents);
            page.set("Resources", Dictionary::new());
            if rotate != 0 {
                page.set("Rotate", rotate);
            }
            if let Some(crop) = crop {
                let [x, y, cw, ch] = crop;
                page.set("CropBox", rect([x, y, x + cw, y + ch]));
            }
            kids.push(Object::Reference(doc.add_object(page)));
        }
        let mut node = Dictionary::new();
        node.set("Type", Object::Name(b"Pages".to_vec()));
        node.set("Count", kids.len() as i64);
        node.set("Kids", kids);
        doc.objects.insert(tree, Object::Dictionary(node));
        finish(doc, tree, title, user, owner)
    }

    /// Adds the catalog (and title) to `doc`, whose page tree is `tree`,
    /// encrypts it when an owner password is given, and saves it.
    pub(crate) fn finish(
        mut doc: Document,
        tree: lopdf::ObjectId,
        title: Option<&str>,
        user: Option<&str>,
        owner: Option<&str>,
    ) -> Vec<u8> {
        let mut catalog = Dictionary::new();
        catalog.set("Type", Object::Name(b"Catalog".to_vec()));
        catalog.set("Pages", tree);
        let catalog = doc.add_object(catalog);
        doc.trailer.set("Root", catalog);
        if let Some(title) = title {
            let mut info = Dictionary::new();
            info.set("Title", Object::string_literal(title));
            let info = doc.add_object(info);
            doc.trailer.set("Info", info);
        }
        if let Some(owner) = owner {
            let id = Object::String(
                b"0123456789abcdef".to_vec(),
                lopdf::StringFormat::Hexadecimal,
            );
            doc.trailer.set("ID", vec![id.clone(), id]);
            let version = EncryptionVersion::V2 {
                document: &doc,
                owner_password: owner,
                user_password: user.unwrap_or(""),
                key_length: 128,
                permissions: Permissions::PRINTABLE,
            };
            let state = EncryptionState::try_from(version).expect("an encryption");
            doc.encrypt(&state).expect("encrypted");
        }
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("saved");
        bytes
    }

    pub(crate) fn rect(r: [f64; 4]) -> Object {
        Object::Array(r.iter().map(|&v| Object::Real(v as f32)).collect())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lopdf::{Dictionary, Document, Object};

    use super::test_support::{finish, fixture, owner_locked, rect, titled};
    use super::*;

    fn source(bytes: Vec<u8>) -> Source {
        let doc = Document::load_mem(&bytes).unwrap();
        Source {
            path: "test.pdf".into(),
            page_count: doc.get_pages().len(),
            bytes: Arc::from(bytes),
        }
    }

    fn page(source: usize, page: usize, turns: u8) -> PageRef {
        PageRef {
            source,
            page,
            turns,
        }
    }

    fn read(bytes: &[u8]) -> Document {
        Document::load_mem(bytes).expect("a PDF")
    }

    fn page_dict(doc: &Document, index: usize) -> &Dictionary {
        let id = doc.get_pages()[&(index as u32 + 1)];
        doc.get_dictionary(id).unwrap()
    }

    /// Each page's media box width and height.
    fn sizes(doc: &Document) -> Vec<(f32, f32)> {
        (0..doc.get_pages().len())
            .map(|i| {
                let media = page_dict(doc, i)
                    .get(b"MediaBox")
                    .unwrap()
                    .as_array()
                    .unwrap();
                let v = |i: usize| media[i].as_float().unwrap();
                (v(2) - v(0), v(3) - v(1))
            })
            .collect()
    }

    fn rotation(doc: &Document, index: usize) -> i64 {
        page_dict(doc, index)
            .get(b"Rotate")
            .unwrap()
            .as_i64()
            .unwrap()
    }

    #[test]
    fn writes_pages_in_order() {
        let a = source(fixture(&[(100.0, 200.0, 0), (300.0, 400.0, 0)], None));
        let b = source(fixture(&[(500.0, 600.0, 0)], None));
        let bytes = assemble(&[a, b], &[page(1, 0, 0), page(0, 1, 0), page(0, 0, 0)]).unwrap();
        assert_eq!(
            sizes(&read(&bytes)),
            [(500.0, 600.0), (300.0, 400.0), (100.0, 200.0)]
        );
    }

    #[test]
    fn writes_the_same_page_twice() {
        let a = source(fixture(&[(100.0, 200.0, 0)], None));
        let bytes = assemble(&[a], &[page(0, 0, 0), page(0, 0, 1)]).unwrap();
        let doc = read(&bytes);
        assert_eq!(doc.get_pages().len(), 2);
        assert_eq!((rotation(&doc, 0), rotation(&doc, 1)), (0, 90));
        // The two pages share one copy of their contents.
        let contents = |i| {
            page_dict(&doc, i)
                .get(b"Contents")
                .unwrap()
                .as_reference()
                .unwrap()
        };
        assert_eq!(contents(0), contents(1));
    }

    #[test]
    fn keeps_the_anchor_title() {
        let a = source(titled(&[(100.0, 100.0, 0)], "First"));
        let b = source(titled(&[(100.0, 100.0, 0)], "Second"));
        let bytes = assemble(&[a, b], &[page(1, 0, 0), page(0, 0, 0)]).unwrap();
        let doc = read(&bytes);
        let info = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
        let title = doc.get_dictionary(info).unwrap().get(b"Title").unwrap();
        assert_eq!(title.as_str().unwrap(), b"Second");
    }

    #[test]
    fn adds_turns_to_the_page_own_rotation() {
        let a = source(fixture(&[(100.0, 200.0, 90), (100.0, 200.0, 270)], None));
        let bytes = assemble(&[a], &[page(0, 0, 1), page(0, 1, 2), page(0, 0, 0)]).unwrap();
        let doc = read(&bytes);
        assert_eq!(
            (rotation(&doc, 0), rotation(&doc, 1), rotation(&doc, 2)),
            (180, 90, 90)
        );
    }

    #[test]
    fn rejects_a_page_out_of_range() {
        let a = source(fixture(&[(100.0, 100.0, 0)], None));
        assert!(matches!(
            assemble(&[a], &[page(0, 3, 0)]),
            Err(PdfError::Backend(_))
        ));
        assert!(matches!(assemble(&[], &[]), Err(PdfError::Backend(_))));
    }

    #[test]
    fn a_locked_source_is_locked() {
        let a = source_unchecked(fixture(&[(100.0, 100.0, 0)], Some("secret")));
        assert!(matches!(
            assemble(&[a], &[page(0, 0, 0)]),
            Err(PdfError::Locked)
        ));
    }

    #[test]
    fn a_source_that_only_restricts_changes_is_written_open() {
        let a = source(owner_locked(&[(100.0, 200.0, 0)], "owner"));
        let bytes = assemble(&[a], &[page(0, 0, 0)]).unwrap();
        let doc = read(&bytes);
        assert!(!doc.is_encrypted());
        assert_eq!(sizes(&doc), [(100.0, 200.0)]);
        let contents = page_dict(&doc, 0)
            .get(b"Contents")
            .unwrap()
            .as_reference()
            .unwrap();
        let stream = doc.get_object(contents).unwrap().as_stream().unwrap();
        assert!(stream.content.starts_with(b"1 0 0 rg"));
    }

    /// A source the test's own reader cannot count pages of (it is locked).
    fn source_unchecked(bytes: Vec<u8>) -> Source {
        Source {
            path: "locked.pdf".into(),
            page_count: 1,
            bytes: Arc::from(bytes),
        }
    }

    /// One page under a tree node holding its media box, resources and
    /// rotation, with a link to itself and one to a second page; the node
    /// also names itself as its own parent.
    fn inheriting() -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let tree = doc.new_object_id();
        let font = doc.add_object({
            let mut font = Dictionary::new();
            font.set("Type", Object::Name(b"Font".to_vec()));
            font.set("Subtype", Object::Name(b"Type1".to_vec()));
            font.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
            font
        });
        let first = doc.new_object_id();
        let second = doc.new_object_id();
        let link = |to| {
            let mut link = Dictionary::new();
            link.set("Type", Object::Name(b"Annot".to_vec()));
            link.set("Subtype", Object::Name(b"Link".to_vec()));
            link.set("Rect", rect([0.0, 0.0, 10.0, 10.0]));
            link.set(
                "Dest",
                vec![Object::Reference(to), Object::Name(b"Fit".to_vec())],
            );
            link
        };
        let to_first = doc.add_object(link(first));
        let to_second = doc.add_object(link(second));
        for (id, annots) in [
            (first, vec![to_first.into(), to_second.into()]),
            (second, vec![]),
        ] {
            let mut page = Dictionary::new();
            page.set("Type", Object::Name(b"Page".to_vec()));
            page.set("Parent", tree);
            page.set("Annots", Object::Array(annots));
            doc.objects.insert(id, Object::Dictionary(page));
        }
        let mut node = Dictionary::new();
        node.set("Type", Object::Name(b"Pages".to_vec()));
        node.set("Parent", tree);
        node.set("Count", 2);
        node.set(
            "Kids",
            vec![Object::Reference(first), Object::Reference(second)],
        );
        node.set("MediaBox", rect([0.0, 0.0, 300.0, 500.0]));
        node.set("Rotate", 90);
        let mut fonts = Dictionary::new();
        fonts.set("F1", font);
        let mut resources = Dictionary::new();
        resources.set("Font", fonts);
        node.set("Resources", resources);
        doc.objects.insert(tree, Object::Dictionary(node));
        finish(doc, tree, None, None, None)
    }

    #[test]
    fn a_page_keeps_what_it_inherits() {
        let a = source(inheriting());
        let bytes = assemble(&[a], &[page(0, 0, 1)]).unwrap();
        let doc = read(&bytes);
        assert_eq!(sizes(&doc), [(300.0, 500.0)]);
        assert_eq!(rotation(&doc, 0), 180);
        let resources = page_dict(&doc, 0)
            .get(b"Resources")
            .unwrap()
            .as_dict()
            .unwrap();
        let font = resources
            .get(b"Font")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"F1")
            .unwrap();
        let font = doc.get_dictionary(font.as_reference().unwrap()).unwrap();
        assert_eq!(
            font.get(b"BaseFont").unwrap().as_name().unwrap(),
            b"Helvetica"
        );
    }

    #[test]
    fn links_reach_written_pages_only() {
        let a = source(inheriting());
        let bytes = assemble(&[a], &[page(0, 0, 0)]).unwrap();
        let doc = read(&bytes);
        let written = doc.get_pages()[&1];
        let annots = page_dict(&doc, 0)
            .get(b"Annots")
            .unwrap()
            .as_array()
            .unwrap();
        let dest = |i: usize| {
            let link = doc
                .get_dictionary(annots[i].as_reference().unwrap())
                .unwrap();
            link.get(b"Dest").unwrap().as_array().unwrap()[0].clone()
        };
        assert_eq!(dest(0), Object::Reference(written));
        assert_eq!(dest(1), Object::Null);
        // Only the written page, not the source's second one.
        assert_eq!(doc.get_pages().len(), 1);
    }
}
