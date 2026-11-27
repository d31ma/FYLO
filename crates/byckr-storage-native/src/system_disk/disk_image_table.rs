use std::path::{Path, PathBuf};

use super::bsd_disk_name::BsdDiskName;

/// The disk images attached on this Mac, read from `hdiutil info -plist`
/// (ADR 0032 section 2.2): each image's file and the devices it presents.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DiskImageTable {
    images: Vec<AttachedImage>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct AttachedImage {
    path: Option<PathBuf>,
    devices: Vec<String>,
}

/// One open element of the property list while it is read.
enum Frame {
    Dict { key: Option<String>, image: bool },
    Array { images: bool },
}

impl DiskImageTable {
    const IMAGES_KEY: &'static str = "images";
    const IMAGE_PATH_KEY: &'static str = "image-path";
    const DEVICE_KEY: &'static str = "dev-entry";
    const PLIST_TAG: &'static str = "<plist";
    const ENTITIES: [(&'static str, &'static str); 5] = [
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&apos;", "'"),
        ("&amp;", "&"),
    ];

    /// Read the property list `hdiutil info -plist` prints.
    ///
    /// # Errors
    ///
    /// Returns a message when the text is not such a property list.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        if !text.contains(Self::PLIST_TAG) {
            return Err("output could not be read: it is not a property list".to_owned());
        }
        let mut reader = TableReader::default();
        reader.read(text)?;
        reader.finish()
    }

    /// The file of the image that presents `whole_disk` (`disk9`), if any.
    pub(crate) fn backing_of(&self, whole_disk: &str) -> Option<&Path> {
        self.images
            .iter()
            .find(|image| Self::presents(image, whole_disk))
            .and_then(|image| image.path.as_deref())
    }

    /// Whether the image that presents `whole_disk` has no file name.
    pub(crate) fn presents_without_file(&self, whole_disk: &str) -> bool {
        self.images
            .iter()
            .any(|image| image.path.is_none() && Self::presents(image, whole_disk))
    }

    fn presents(image: &AttachedImage, whole_disk: &str) -> bool {
        image
            .devices
            .iter()
            .any(|device| BsdDiskName::whole_disk(device).is_ok_and(|disk| disk == whole_disk))
    }

    fn unescape(text: &str) -> String {
        Self::ENTITIES
            .iter()
            .fold(text.to_owned(), |text, (entity, character)| {
                text.replace(entity, character)
            })
    }
}

#[derive(Default)]
struct TableReader {
    stack: Vec<Frame>,
    images: Vec<AttachedImage>,
    saw_images: bool,
}

impl TableReader {
    fn read(&mut self, text: &str) -> Result<(), String> {
        let mut rest = text;
        while let Some(start) = rest.find('<') {
            let end = rest[start..]
                .find('>')
                .map(|offset| start + offset)
                .ok_or("output could not be read: an open tag")?;
            let tag = &rest[start + 1..end];
            rest = &rest[end + 1..];
            rest = self.tag(tag, rest)?;
        }
        Ok(())
    }

    fn finish(self) -> Result<DiskImageTable, String> {
        if !self.saw_images || !self.stack.is_empty() {
            return Err("output could not be read: no complete images list".to_owned());
        }
        Ok(DiskImageTable {
            images: self.images,
        })
    }

    /// Handle one tag; text elements consume their text from `rest`.
    fn tag<'a>(&mut self, tag: &str, rest: &'a str) -> Result<&'a str, String> {
        match tag {
            "dict" => self.open_dict(),
            "array" => self.open_array(),
            "/dict" | "/array" => self.close()?,
            "key" | "string" => return self.text_element(tag, rest),
            _ => self.other(tag),
        }
        Ok(rest)
    }

    fn open_dict(&mut self) {
        let image = matches!(self.stack.last(), Some(Frame::Array { images: true }));
        self.take_key();
        if image {
            self.images.push(AttachedImage::default());
        }
        self.stack.push(Frame::Dict { key: None, image });
    }

    fn open_array(&mut self) {
        let images =
            self.stack.len() == 1 && self.take_key().as_deref() == Some(DiskImageTable::IMAGES_KEY);
        self.saw_images |= images;
        self.stack.push(Frame::Array { images });
    }

    fn close(&mut self) -> Result<(), String> {
        self.stack
            .pop()
            .map(drop)
            .ok_or_else(|| "output could not be read: an unmatched closing tag".to_owned())
    }

    /// `<array/>`, `<true/>`, numbers and the header: they end a key.
    fn other(&mut self, tag: &str) {
        let empty_images = tag == "array/"
            && self.stack.len() == 1
            && self.peek_key() == Some(DiskImageTable::IMAGES_KEY);
        self.saw_images |= empty_images;
        if !tag.starts_with('/') && !tag.starts_with('?') && !tag.starts_with('!') {
            self.take_key();
        }
    }

    fn text_element<'a>(&mut self, tag: &str, rest: &'a str) -> Result<&'a str, String> {
        let closing = format!("</{tag}>");
        let end = rest
            .find(&closing)
            .ok_or_else(|| format!("output could not be read: an unclosed <{tag}>"))?;
        let text = DiskImageTable::unescape(&rest[..end]);
        if tag == "key" {
            self.set_key(text);
        } else {
            self.value(text);
        }
        Ok(&rest[end + closing.len()..])
    }

    fn value(&mut self, text: String) {
        let key = self.take_key();
        let image_root = matches!(self.stack.last(), Some(Frame::Dict { image: true, .. }));
        let Some(image) = self.images.last_mut() else {
            return;
        };
        match key.as_deref() {
            Some(DiskImageTable::IMAGE_PATH_KEY) if image_root => {
                image.path = Some(PathBuf::from(text));
            }
            Some(DiskImageTable::DEVICE_KEY) => image.devices.push(text),
            _ => {}
        }
    }

    fn set_key(&mut self, text: String) {
        if let Some(Frame::Dict { key, .. }) = self.stack.last_mut() {
            *key = Some(text);
        }
    }

    fn peek_key(&self) -> Option<&str> {
        match self.stack.last() {
            Some(Frame::Dict { key, .. }) => key.as_deref(),
            _ => None,
        }
    }

    fn take_key(&mut self) -> Option<String> {
        match self.stack.last_mut() {
            Some(Frame::Dict { key, .. }) => key.take(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DiskImageTable;
    use std::path::Path;

    const ATTACHED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>framework</key>
	<string>710</string>
	<key>images</key>
	<array>
		<dict>
			<key>autodiskmount</key>
			<false/>
			<key>image-path</key>
			<string>/Volumes/ATLAS/Mesail/mesail-fylo-test.sparsebundle</string>
			<key>system-entities</key>
			<array>
				<dict>
					<key>content-hint</key>
					<string>GUID_partition_scheme</string>
					<key>dev-entry</key>
					<string>/dev/disk8</string>
				</dict>
				<dict>
					<key>dev-entry</key>
					<string>/dev/disk9s1</string>
					<key>mount-point</key>
					<string>/Users/me/Developer/AtlasFylo/mesail</string>
				</dict>
			</array>
			<key>writeable</key>
			<true/>
		</dict>
		<dict>
			<key>system-entities</key>
			<array>
				<dict>
					<key>dev-entry</key>
					<string>/dev/disk11s1</string>
				</dict>
			</array>
			<key>image-path</key>
			<string>/Users/me/a &amp; b.dmg</string>
		</dict>
	</array>
</dict>
</plist>
"#;

    #[test]
    fn each_image_names_its_file_and_its_disks() {
        let table = DiskImageTable::parse(ATTACHED).unwrap();
        let mesail = Path::new("/Volumes/ATLAS/Mesail/mesail-fylo-test.sparsebundle");
        assert_eq!(table.backing_of("disk9"), Some(mesail));
        assert_eq!(table.backing_of("disk8"), Some(mesail));
        assert_eq!(
            table.backing_of("disk11"),
            Some(Path::new("/Users/me/a & b.dmg"))
        );
    }

    #[test]
    fn a_disk_no_image_presents_has_no_file() {
        let table = DiskImageTable::parse(ATTACHED).unwrap();
        assert_eq!(table.backing_of("disk3"), None);
        assert_eq!(table.backing_of("disk1"), None);
        assert!(!table.presents_without_file("disk9"));
    }

    #[test]
    fn no_attached_image_is_an_empty_table() {
        let text = "<plist version=\"1.0\"><dict><key>framework</key><string>710</string><key>images</key><array/></dict></plist>";
        let table = DiskImageTable::parse(text).unwrap();
        assert_eq!(table.backing_of("disk9"), None);
    }

    #[test]
    fn output_that_is_not_the_list_cannot_be_read() {
        assert!(DiskImageTable::parse("hdiutil: info failed - no such file").is_err());
        assert!(
            DiskImageTable::parse("<plist><dict><key>other</key><string>x</string></dict></plist>")
                .is_err()
        );
        assert!(DiskImageTable::parse("<plist><dict><key>images</key><array><dict>").is_err());
        assert!(
            DiskImageTable::parse(
                "<plist><dict><key>images</key><array><dict><key>image-path</key><string>/x"
            )
            .is_err()
        );
    }
}
