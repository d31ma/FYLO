#![no_main]

use fylo_format::{
    ContainerCheckpointSlot, ContainerCollectionHeader, ContainerExtentHeader,
    ContainerExtentMapPage, ContainerGenerationSlot, ContainerSuperblock,
};
use libfuzzer_sys::fuzz_target;

// Every `.vdbx` structure decoder (ADR 0016) must reject arbitrary bytes
// without panicking, and whatever it accepts must re-encode to bytes that
// decode to the same value.
fuzz_target!(|input: &[u8]| {
    if let Ok(superblock) = ContainerSuperblock::decode(input) {
        assert_eq!(
            ContainerSuperblock::decode(&superblock.encode()).as_ref(),
            Ok(&superblock)
        );
    }
    let half = input.len() / 2;
    let _ = ContainerSuperblock::newest(&input[..half], &input[half..]);

    if let Ok(header) = ContainerExtentHeader::decode(input) {
        assert_eq!(ContainerExtentHeader::decode(&header.encode()), Ok(header));
    }
    if let Ok(page) = ContainerExtentMapPage::decode(input) {
        let encoded = page.encode().expect("a decoded page re-encodes");
        assert_eq!(ContainerExtentMapPage::decode(&encoded).as_ref(), Ok(&page));
    }
    if let Ok(collection) = ContainerCollectionHeader::decode(input) {
        let encoded = collection.encode().expect("a decoded header re-encodes");
        assert_eq!(
            ContainerCollectionHeader::decode(&encoded).as_ref(),
            Ok(&collection)
        );
    }
    if let Ok(slot) = ContainerGenerationSlot::decode(input) {
        assert_eq!(ContainerGenerationSlot::decode(&slot.encode()), Ok(slot));
    }
    let _ = ContainerGenerationSlot::newest(input);
    if let Ok(slot) = ContainerCheckpointSlot::decode(input) {
        assert_eq!(ContainerCheckpointSlot::decode(&slot.encode()), Ok(slot));
    }
    let _ = ContainerCheckpointSlot::newest(input);
});
