use super::super::query::parse_remote_branch_heads;

#[test]
fn parses_heads_without_remote_ref_prefixes() {
    let heads = parse_remote_branch_heads(
        "1111111111111111111111111111111111111111\trefs/heads/main\n\
         2222222222222222222222222222222222222222\trefs/heads/feature/x\n\
         malformed\n",
    );

    assert_eq!(heads.len(), 2);
    assert_eq!(
        heads.get("main").map(String::as_str),
        Some("1111111111111111111111111111111111111111")
    );
    assert_eq!(
        heads.get("feature/x").map(String::as_str),
        Some("2222222222222222222222222222222222222222")
    );
}
