mod relative_paths {

    use crate::task::artifacts::*;

    #[test]
    fn artifact_path_validation_rejects_absolute_and_parent_paths() {
        assert!(validate_relative_artifact_path("files/result.txt").is_ok());
        assert!(validate_relative_artifact_path("/tmp/result.txt").is_err());
        assert!(validate_relative_artifact_path("../result.txt").is_err());
        assert!(validate_relative_artifact_path("files/../result.txt").is_err());
        assert!(validate_relative_artifact_path(r"files\result.txt").is_err());
        assert!(validate_relative_artifact_path("./result.txt").is_err());
        assert!(validate_relative_artifact_path("   ").is_err());
    }
}

mod presentation {
    use crate::task::{
        ArtifactPresentation, artifact_presentation, inline_safe_artifact_media_type,
        is_inline_image_media_type,
    };

    #[test]
    fn inline_allowlist_covers_raster_images_and_excludes_active_content() {
        for media_type in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
            assert!(
                is_inline_image_media_type(media_type),
                "{media_type} should be inline-renderable"
            );
        }
        // Active content: both are viewable formats, both host script.
        assert_eq!(inline_safe_artifact_media_type("image/svg+xml"), None);
        assert_eq!(inline_safe_artifact_media_type("text/html"), None);
        assert!(!is_inline_image_media_type("text/plain"));
    }

    #[test]
    fn a_declared_image_whose_bytes_do_not_match_is_not_presented_as_an_image() {
        // `payload.png` carrying markup: the extension picked the media type,
        // so only the signature check stops a renderer from trusting it.
        assert_eq!(
            artifact_presentation("image/png", b"<html><script>alert(1)</script></html>"),
            ArtifactPresentation::Opaque
        );
    }
}

mod textual_policy {
    use crate::task::is_textual_artifact_media_type;

    #[test]
    fn active_content_is_excluded_from_the_textual_set() {
        for media_type in ["text/html", "image/svg+xml", "application/xhtml+xml"] {
            assert!(
                !is_textual_artifact_media_type(media_type),
                "{media_type} hosts script and must not be classified as plain text"
            );
        }
    }
}
