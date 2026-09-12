use poise::serenity_prelude as serenity;

use crate::{
    config::Data,
    util::{attach_name, cap_file_body, strip_sgr},
    Error,
};

fn is_boo_message(s: &str) -> bool {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == "boo")
}

fn artixy_text(s: &str) -> Option<String> {
    let text = s.trim_end().strip_suffix(".ar")?.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.to_string())
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
    let id = new_message.author.id.get();
    let (owner, blocked) = {
        let a = data.allowed.read().await;
        (id == a.owner, a.blocked.contains(&id))
    };
    if blocked {
        return Ok(());
    }
    if !owner {
        if is_boo_message(&new_message.content) {
            let _ = new_message.reply(&ctx.http, "boo on you! :3").await;
        }
        return Ok(());
    }
    let Some(text) = artixy_text(&new_message.content) else {
        return Ok(());
    };
    let mut deleted = false;
    for _ in 0..3 {
        match new_message.delete(&ctx.http).await {
            Ok(_) => {
                deleted = true;
                break;
            }
            Err(e) => {
                eprintln!(
                    "artixy-say: delete attempt failed for {} in {}: {}",
                    new_message.id.get(),
                    new_message.channel_id.get(),
                    e
                );
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
    if !deleted {
        eprintln!(
            "artixy-say: giving up deleting {} (needs Manage Messages there)",
            new_message.id.get()
        );
    }
    if text.chars().count() <= 2000 {
        match &new_message.referenced_message {
            Some(target) => {
                let _ = target.reply(&ctx.http, &text).await;
            }
            None => {
                let _ = new_message.channel_id.say(&ctx.http, &text).await;
            }
        }
    } else {
        let att = serenity::CreateAttachment::bytes(
            cap_file_body(&strip_sgr(&text)).into_bytes(),
            attach_name(&text),
        );
        let _ = new_message
            .channel_id
            .send_message(&ctx.http, serenity::CreateMessage::new().add_file(att))
            .await;
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
    fn artixy_suffix_returns_plain_text() {
        assert_eq!(artixy_text("hello .ar"), Some("hello".into()));
        assert_eq!(artixy_text("hello .ar   "), Some("hello".into()));
        assert_eq!(artixy_text("a.ar"), Some("a".into()));
        assert_eq!(artixy_text(".ar"), None);
        assert_eq!(artixy_text("   .ar  "), None);
        assert_eq!(artixy_text(".ar hello"), None);
        assert_eq!(artixy_text("hello"), None);
        assert_eq!(artixy_text(""), None);
        assert_eq!(artixy_text(".AR"), None);
        assert_eq!(artixy_text("hello .artixy"), None);
    }
}
