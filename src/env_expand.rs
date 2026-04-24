//! Shared `${VAR}` expansion used by every string-shaped config field and
//! by `[agents.*].env` values at spawn time. Kept in its own tiny module so
//! integration tests that only pull in `src/acp/` (via `#[path = ...]`)
//! don't drag the whole `config` module graph along for one function.
//!
//! Semantics:
//! - `${VAR}` anywhere in the string is replaced with the env var's value.
//! - Unknown variables resolve to an empty string.
//! - `$$` escapes a literal `$`.
//! - A `$` not followed by `{` or `$` is preserved as-is.
//! - An unterminated `${...` (missing `}`) is preserved literally, so a
//!   malformed config fails later validation instead of silently emptying.

pub fn expand_str(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('$') => {
                chars.next();
                out.push('$');
            }
            Some('{') => {
                chars.next();
                let mut name = String::new();
                let mut closed = false;
                for nc in chars.by_ref() {
                    if nc == '}' {
                        closed = true;
                        break;
                    }
                    name.push(nc);
                }
                if closed {
                    let val = std::env::var(&name).unwrap_or_default();
                    out.push_str(&val);
                } else {
                    out.push('$');
                    out.push('{');
                    out.push_str(&name);
                }
            }
            _ => out.push('$'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_value_expansion() {
        std::env::set_var("HAMMURABI_EXP_WHOLE", "abc");
        assert_eq!(expand_str("${HAMMURABI_EXP_WHOLE}"), "abc");
        std::env::remove_var("HAMMURABI_EXP_WHOLE");
    }

    #[test]
    fn mid_string_expansion() {
        std::env::set_var("HAMMURABI_EXP_MID", "value");
        assert_eq!(
            expand_str("Bearer ${HAMMURABI_EXP_MID} token"),
            "Bearer value token"
        );
        std::env::remove_var("HAMMURABI_EXP_MID");
    }

    #[test]
    fn unknown_var_empty() {
        std::env::remove_var("HAMMURABI_EXP_MISSING");
        assert_eq!(expand_str("[${HAMMURABI_EXP_MISSING}]"), "[]");
    }

    #[test]
    fn dollar_dollar_escape() {
        assert_eq!(expand_str("cost: $$5"), "cost: $5");
    }

    #[test]
    fn lone_dollar_preserved() {
        assert_eq!(expand_str("price $100"), "price $100");
    }

    #[test]
    fn unterminated_preserved() {
        assert_eq!(expand_str("broken ${UNCLOSED"), "broken ${UNCLOSED");
    }

    #[test]
    fn multiple_vars_in_one_string() {
        std::env::set_var("HAMMURABI_EXP_A", "foo");
        std::env::set_var("HAMMURABI_EXP_B", "bar");
        assert_eq!(
            expand_str("${HAMMURABI_EXP_A}-${HAMMURABI_EXP_B}"),
            "foo-bar"
        );
        std::env::remove_var("HAMMURABI_EXP_A");
        std::env::remove_var("HAMMURABI_EXP_B");
    }
}
