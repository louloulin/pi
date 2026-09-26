//! HTML responses served by the OAuth callback server.
//!
//! Port of `packages/ai/src/auth/oauth/oauth-page.ts`. The bodies are
//! deliberately tiny and self-contained: the user lands on them in the
//! browser tab the OAuth provider opens after the user accepts the
//! authorization request.

/// Success HTML rendered into the browser tab when the OAuth callback
/// completes (`oauthSuccessHtml` upstream).
pub fn oauth_success_html(message: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Sign-in complete</title>\
         <style>body{{font-family:system-ui,sans-serif;display:grid;place-items:center;\
         min-height:100vh;margin:0;background:#0b0b0b;color:#e6e6e6}}\
         .card{{padding:1.5rem 2rem;border-radius:.75rem;background:#161616;\
         border:1px solid #2a2a2a;max-width:28rem;text-align:center}}\
         h1{{font-size:1.05rem;margin:0 0 .5rem 0;font-weight:600}}\
         p{{margin:0;font-size:.9rem;color:#a8a8a8}}</style></head>\
         <body><div class=\"card\"><h1>Sign-in complete</h1><p>{message}</p></div></body></html>"
    )
}

/// Error HTML rendered into the browser tab when the OAuth callback
/// fails (`oauthErrorHtml` upstream).
pub fn oauth_error_html(message: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Sign-in failed</title>\
         <style>body{{font-family:system-ui,sans-serif;display:grid;place-items:center;\
         min-height:100vh;margin:0;background:#0b0b0b;color:#e6e6e6}}\
         .card{{padding:1.5rem 2rem;border-radius:.75rem;background:#161616;\
         border:1px solid #2a2a2a;max-width:28rem;text-align:center}}\
         h1{{font-size:1.05rem;margin:0 0 .5rem 0;font-weight:600;color:#e57373}}\
         p{{margin:0;font-size:.9rem;color:#a8a8a8}}</style></head>\
         <body><div class=\"card\"><h1>Sign-in failed</h1><p>{message}</p></div></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_html_embeds_the_message() {
        let html = oauth_success_html("You can close this window.");
        assert!(html.contains("Sign-in complete"));
        assert!(html.contains("You can close this window."));
        assert!(html.contains("<!doctype html>"));
    }

    #[test]
    fn error_html_embeds_the_message() {
        let html = oauth_error_html("State mismatch");
        assert!(html.contains("Sign-in failed"));
        assert!(html.contains("State mismatch"));
    }
}