use poise::serenity_prelude as serenity;

use crate::{config::Data, Error};

fn is_boo_message(s: &str) -> bool {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == "boo")
}

fn strip_artixy_suffix(s: &str) -> Option<String> {
    let text = s.trim_end().strip_suffix(".artixy")?.trim().to_string();
    if text.is_empty() || text.chars().count() > 2000 {
        return None;
    }
    Some(text)
}

pub(crate) async fn event_handler(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) -> Result<(), Error> {
    let serenity::FullEvent::Message { new_message } = event else {
        return Ok(());
    };
    if new_message.author.bot {
        return Ok(());
    }
    let owner = new_message.author.id.get() == data.allowed.read().await.owner;
    if !owner {
        if is_boo_message(&new_message.content) {
            let _ = new_message.reply(&ctx.http, "boo on you! :3").await;
        }
        return Ok(());
    }
    let Some(text) = strip_artixy_suffix(&new_message.content) else {
        return Ok(());
    };
    let posted_ok = match &new_message.referenced_message {
        Some(target) => target.reply(&ctx.http, &text).await.is_ok(),
        None => new_message.channel_id.say(&ctx.http, &text).await.is_ok(),
    };
    if posted_ok {
        if let Err(e) = new_message.delete(&ctx.http).await {
            eprintln!("artixy-say: posted but failed to delete original: {}", e);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boo_matches_whole_word_case_insensitive() {
        assert!(is_boo_message("boo"));
        assert!(is_boo_message("Boo"));
        assert!(is_boo_message("BOO!"));
        assert!(is_boo_message("well, boo."));
        assert!(!is_boo_message("book"));
        assert!(!is_boo_message("bamboo"));
        assert!(!is_boo_message("taboo"));
        assert!(!is_boo_message(""));
        assert!(!is_boo_message("hello there"));
    }

    #[test]
    fn artixy_suffix_strips_only_at_end() {
        assert_eq!(strip_artixy_suffix("hello .artixy"), Some("hello".into()));
        assert_eq!(strip_artixy_suffix("hello .artixy   "), Some("hello".into()));
        assert_eq!(strip_artixy_suffix("a.artixy"), Some("a".into()));
        assert_eq!(strip_artixy_suffix(".artixy"), None);
        assert_eq!(strip_artixy_suffix("   .artixy  "), None);
        assert_eq!(strip_artixy_suffix(".artixy hello"), None);
        assert_eq!(strip_artixy_suffix("hello"), None);
        assert_eq!(strip_artixy_suffix(""), None);
        assert_eq!(strip_artixy_suffix(".ARTIXY"), None);
    }
}
