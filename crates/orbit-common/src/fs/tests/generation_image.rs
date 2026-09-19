use super::{macho_commands_length, macho_uuid};

#[test]
fn native_image_uuid_parser_rejects_missing_truncated_and_unknown_images() {
    let mut header = [0u8; 32];
    assert!(macho_commands_length(&header).is_err());
    header[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
    header[20..24].copy_from_slice(&24u32.to_le_bytes());
    assert_eq!(macho_commands_length(&header).expect("native header"), 24);
    header[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(macho_commands_length(&header).is_err());
    let mut commands = [0u8; 24];
    assert!(macho_uuid(&commands).is_err());
    commands[..4].copy_from_slice(&0x1bu32.to_le_bytes());
    commands[4..8].copy_from_slice(&24u32.to_le_bytes());
    commands[8..].fill(17);
    assert_eq!(macho_uuid(&commands).expect("image UUID"), [17; 16]);
    assert!(macho_uuid(&commands[..23]).is_err());
}
