use poise::serenity_prelude as serenity;

use crate::{config::Data, Error};

fn is_boo_message(s: &str) -> bool {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == "boo")
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
    if new_message.author.id.get() == data.allowed.read().await.owner {
        return Ok(());
    }
    if is_boo_message(&new_message.content) {
        let _ = new_message.reply(&ctx.http, "boo on you! :3").await;
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
}
