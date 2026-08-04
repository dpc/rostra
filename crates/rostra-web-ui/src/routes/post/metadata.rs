use rostra_djot::extract::SocialExcerpt;

/// The title and description shared by post social-metadata formats.
pub(crate) struct SocialMetadata {
    /// The page, Open Graph, and Twitter title.
    pub title: String,
    /// The Open Graph, Twitter, and JSON-LD body excerpt.
    pub description: String,
}

const DESCRIPTION_MAX_CHARS: usize = 600;

/// Return a non-empty display name, falling back to the canonical short ID.
pub(crate) fn display_name_or_short_id(display_name: Option<&str>, short_id: &str) -> String {
    display_name
        .map(str::trim)
        .filter(|display_name| !display_name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| short_id.to_owned())
}

/// Build shared social metadata from a post's extracted content and identity
/// context.
pub(crate) fn social_metadata(
    excerpt: &SocialExcerpt,
    author_name: &str,
    is_reply: bool,
    reply_target_name: Option<&str>,
) -> SocialMetadata {
    let title = excerpt.first_heading.clone().unwrap_or_else(|| {
        if is_reply {
            reply_target_name.map_or_else(
                || format!("{} reply to a post", possessive(author_name)),
                |reply_target_name| {
                    format!(
                        "{} reply to {} post",
                        possessive(author_name),
                        possessive(reply_target_name)
                    )
                },
            )
        } else {
            format!("{} post on Rostra", possessive(author_name))
        }
    });

    SocialMetadata {
        title,
        description: social_description(&excerpt.paragraphs),
    }
}

/// Build the bounded social description from normalized paragraph blocks.
pub(crate) fn social_description(paragraphs: &[String]) -> String {
    truncate_at_unicode_whitespace(&paragraphs.join("\n\n"), DESCRIPTION_MAX_CHARS)
}

fn possessive(name: &str) -> String {
    if name.ends_with(['s', 'S']) {
        format!("{name}'")
    } else {
        format!("{name}'s")
    }
}

fn truncate_at_unicode_whitespace(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }

    let truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    let boundary = truncated
        .char_indices()
        .rev()
        .find_map(|(index, character)| character.is_whitespace().then_some(index));
    let text = boundary
        .map(|index| truncated[..index].trim_end())
        .filter(|text| !text.is_empty())
        .unwrap_or(&truncated);

    format!("{text}\u{2026}")
}

#[cfg(test)]
mod tests;
