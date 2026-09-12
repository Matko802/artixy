use poise::serenity_prelude as serenity;

use crate::{config::Data, Error};

fn is_boo_message(s: &str) -> bool {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == "boo")
}

fn artixy_fenced(s: &str) -> Option<String> {
    let text = s.trim_end().strip_suffix(".artixy")?.trim();
    if text.is_empty() {
        return None;
    }
    let fenced = format!("```ansi\n{}\n```", text);
    if fenced.chars().count() > 2000 {
        return None;
    }
    Some(fenced)
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
    let Some(fenced) = artixy_fenced(&new_message.content) else {
        return Ok(());
    };
    let posted_ok = match &new_message.referenced_message {
        Some(target) => target.reply(&ctx.http, &fenced).await.is_ok(),
        None => new_message.channel_id.say(&ctx.http, &fenced).await.is_ok(),
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
    fn artixy_suffix_fences_ansi() {
        assert_eq!(
            artixy_fenced("hello .artixy"),
            Some("```ansi\nhello\n```".into())
        );
        assert_eq!(
            artixy_fenced("hello .artixy   "),
            Some("```ansi\nhello\n```".into())
        );
        assert_eq!(artixy_fenced("a.artixy"), Some("```ansi\na\n```".into()));
        assert_eq!(artixy_fenced(".artixy"), None);
        assert_eq!(artixy_fenced("   .artixy  "), None);
        assert_eq!(artixy_fenced(".artixy hello"), None);
        assert_eq!(artixy_fenced("hello"), None);
        assert_eq!(artixy_fenced(""), None);
        assert_eq!(artixy_fenced(".ARTIXY"), None);
        assert_eq!(artixy_fenced(&format!("{}.artixy", "y".repeat(1990))), None);
        assert!(artixy_fenced(&format!("{}.artixy", "y".repeat(1980))).is_some());
    }
}
