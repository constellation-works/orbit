use super::{ceil_char_boundary, floor_char_boundary};

#[test]
fn boundaries_never_split_a_character() {
    let text = "a€b"; // '€' spans bytes 1..4.

    assert_eq!(floor_char_boundary(text, 2), 1);
    assert_eq!(ceil_char_boundary(text, 2), 4);
    assert_eq!(floor_char_boundary(text, 4), 4);
    assert_eq!(ceil_char_boundary(text, 1), 1);
    assert_eq!(floor_char_boundary(text, 99), text.len());
    assert_eq!(ceil_char_boundary(text, 99), text.len());
}
