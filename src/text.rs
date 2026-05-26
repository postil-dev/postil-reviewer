pub fn limit_text(text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }

    let end = if text.is_char_boundary(limit) {
        limit
    } else {
        (0..limit)
            .rev()
            .find(|index| text.is_char_boundary(*index))
            .unwrap_or(0)
    };

    format!("{}\n\n[diff truncated]", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::limit_text;

    #[test]
    fn truncates_at_utf8_boundary() {
        assert_eq!(limit_text("éclair".to_string(), 1), "\n\n[diff truncated]");
        assert_eq!(
            limit_text("aéclair".to_string(), 2),
            "a\n\n[diff truncated]"
        );
    }
}
