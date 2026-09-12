use poise::serenity_prelude as serenity;

use crate::{
    config::Data,
    util::{attach_name, cap_file_body, strip_sgr},
    webhook::{handle_delete, is_posted_message, post_message},
    Error,
};

fn is_boo_message(s: &str) -> bool {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == "boo")
}

const TAUNT: &str = "purged your message haha";

fn blocked_reply_action(
    ref_author: Option<u64>,
    ref_id: Option<serenity::MessageId>,
    ref_content: &str,
    me: u64,
) -> (bool, bool) {
    let mine = matches!(ref_author, Some(a) if a == me)
        || ref_id.map(|id| is_posted_message(&id)).unwrap_or(false);
    match mine {
        true => (true, ref_content != TAUNT),
        false => (false, false),
    }
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
    if let serenity::FullEvent::MessageDelete {
        deleted_message_id,
        ..
    } = event
    {
        let war = data.settings.read().await.war_mode;
        handle_delete(&ctx.http, *deleted_message_id, war).await;
        return Ok(());
    }
    let serenity::FullEvent::Message { new_message } = event else {
        return Ok(());
    };
    let id = new_message.author.id.get();
    let (owner, blocked, war) = {
        let a = data.allowed.read().await;
        let s = data.settings.read().await;
        (id == a.owner, a.blocked.contains(&id), s.war_mode)
    };
    if blocked && war {
        let me = match ctx.http.get_current_user().await {
            Ok(u) => u.id.get(),
            Err(_) => return Ok(()),
        };
        let (delete, taunt) = match &new_message.referenced_message {
            Some(r) => blocked_reply_action(Some(r.author.id.get()), Some(r.id), &r.content, me),
            None => match &new_message.message_reference {
                Some(r) => match r.message_id {
                    Some(mid) => match new_message.channel_id.message(&ctx.http, mid).await {
                        Ok(orig) => blocked_reply_action(
                            Some(orig.author.id.get()),
                            Some(orig.id),
                            &orig.content,
                            me,
                        ),
                        Err(_) => (false, false),
                    },
                    None => (false, false),
                },
                None => (false, false),
            },
        };
        if delete {
            match new_message.delete(&ctx.http).await {
                Ok(_) => {
                    if taunt {
                        let _ = post_message(&ctx.http, new_message.channel_id, TAUNT.into(), Vec::new()).await;
                    }
                }
                Err(e) => {
                    eprintln!(
                        "guard: failed to delete blocked reply {} in {}: {}",
                        new_message.id.get(),
                        new_message.channel_id.get(),
                        e
                    );
                }
            }
        }
        return Ok(());
    }
    if new_message.author.bot {
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
                let _ = post_message(&ctx.http, new_message.channel_id, text, Vec::new()).await;
            }
        }
    } else {
        let att = (
            attach_name(&text),
            cap_file_body(&strip_sgr(&text)).into_bytes(),
        );
        let _ = post_message(&ctx.http, new_message.channel_id, String::new(), vec![att]).await;
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
    fn blocked_reply_verdicts() {
        assert_eq!(blocked_reply_action(Some(9), None, "trash talk", 9), (true, true));
        assert_eq!(
            blocked_reply_action(Some(9), None, "purged your message haha", 9),
            (true, false)
        );
        assert_eq!(blocked_reply_action(Some(3), None, "trash talk", 9), (false, false));
        assert_eq!(blocked_reply_action(None, None, "trash talk", 9), (false, false));
        assert_eq!(blocked_reply_action(None, None, "", 9), (false, false));
    }

    #[test]
    fn artixy_suffix_returns_plain_text() {        assert_eq!(artixy_text("hello .ar"), Some("hello".into()));
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
