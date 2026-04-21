use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::registry::ToolRegistry;

/// Register web_search, web_fetch, and api_request tools.
pub fn register(registry: &mut ToolRegistry, max_chars: usize) {
    let client = Arc::new(Client::new());

    let c = client.clone();
    let mc = max_chars;
    registry.register(
        "web_search",
        "Search the web using DuckDuckGo. Returns titles, snippets, and URLs.",
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results (default: 5)"
                }
            },
            "required": ["query"]
        }),
        move |args| {
            let c = c.clone();
            async move { web_search(&c, args, mc).await }
        },
    );

    let c = client.clone();
    let mc = max_chars;
    registry.register(
        "web_fetch",
        "Fetch a web page and extract its text content.",
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL to fetch"
                }
            },
            "required": ["url"]
        }),
        move |args| {
            let c = c.clone();
            async move { web_fetch(&c, args, mc).await }
        },
    );

    let c = client.clone();
    let mc = max_chars;
    registry.register(
        "api_request",
        "Make an HTTP request. Returns status, headers, and body.",
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Request URL"
                },
                "method": {
                    "type": "string",
                    "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"],
                    "description": "HTTP method (default: GET)"
                },
                "headers": {
                    "type": "object",
                    "description": "Request headers as key-value pairs"
                },
                "body": {
                    "type": "string",
                    "description": "Request body"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 30)"
                }
            },
            "required": ["url"]
        }),
        move |args| {
            let c = c.clone();
            async move { api_request(&c, args, mc).await }
        },
    );
}

async fn web_search(client: &Client, args: Value, max_chars: usize) -> String {
    let query = match args.get("query").and_then(|q| q.as_str()) {
        Some(q) => q,
        None => return "Error: 'query' parameter required".to_string(),
    };
    let max_results = args
        .get("max_results")
        .and_then(|m| m.as_u64())
        .unwrap_or(5) as usize;

    // Use DuckDuckGo HTML search (no API key required)
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoded(query)
    );

    let resp = match client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (compatible; barebone-agent/0.1)")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return format!("Error: search request failed: {}", e),
    };

    let html = match resp.text().await {
        Ok(t) => t,
        Err(e) => return format!("Error: failed to read response: {}", e),
    };

    // Parse results from DuckDuckGo HTML
    let results = parse_ddg_results(&html, max_results);
    if results.is_empty() {
        return "No results found.".to_string();
    }

    let output = results
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{}. {}\n   {}\n   {}", i + 1, r.title, r.snippet, r.url))
        .collect::<Vec<_>>()
        .join("\n\n");

    truncate(&output, max_chars)
}

async fn web_fetch(client: &Client, args: Value, max_chars: usize) -> String {
    let url = match args.get("url").and_then(|u| u.as_str()) {
        Some(u) => u,
        None => return "Error: 'url' parameter required".to_string(),
    };

    let resp = match client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (compatible; barebone-agent/0.1)")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return format!("Error: fetch failed: {}", e),
    };

    let status = resp.status().as_u16();
    let html = match resp.text().await {
        Ok(t) => t,
        Err(e) => return format!("Error: failed to read response: {}", e),
    };

    // Convert HTML to readable text
    let text = html2text::from_read(html.as_bytes(), 80)
        .unwrap_or_else(|_| html.clone());

    let output = format!("Status: {}\n\n{}", status, text.trim());
    truncate(&output, max_chars)
}

async fn api_request(client: &Client, args: Value, max_chars: usize) -> String {
    let url = match args.get("url").and_then(|u| u.as_str()) {
        Some(u) => u,
        None => return "Error: 'url' parameter required".to_string(),
    };

    let method = args
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or("GET")
        .to_uppercase();

    let timeout_secs = args
        .get("timeout")
        .and_then(|t| t.as_u64())
        .unwrap_or(30);

    let mut req = match method.as_str() {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "PATCH" => client.patch(url),
        "DELETE" => client.delete(url),
        _ => return format!("Error: unsupported method '{}'", method),
    };

    req = req.timeout(std::time::Duration::from_secs(timeout_secs));

    // Add custom headers
    if let Some(headers) = args.get("headers").and_then(|h| h.as_object()) {
        for (key, value) in headers {
            if let Some(v) = value.as_str() {
                req = req.header(key, v);
            }
        }
    }

    // Add body
    if let Some(body) = args.get("body").and_then(|b| b.as_str()) {
        req = req.body(body.to_string());
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return format!("Error: request failed: {}", e),
    };

    let status = resp.status().as_u16();
    let resp_headers: HashMap<String, String> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = resp.text().await.unwrap_or_default();

    let headers_str = resp_headers
        .iter()
        .map(|(k, v)| format!("  {}: {}", k, v))
        .collect::<Vec<_>>()
        .join("\n");

    let output = format!(
        "Status: {}\nHeaders:\n{}\nBody:\n{}",
        status, headers_str, body
    );
    truncate(&output, max_chars)
}

struct SearchResult {
    title: String,
    snippet: String,
    url: String,
}

fn parse_ddg_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // Simple parsing of DuckDuckGo HTML results
    // Results are in <div class="result"> blocks
    for chunk in html.split("class=\"result__a\"").skip(1).take(max_results) {
        let title = extract_between(chunk, ">", "</a>")
            .map(|t| strip_html_tags(t))
            .unwrap_or_default();

        let url = extract_between(chunk, "href=\"", "\"")
            .map(|u| decode_ddg_url(u))
            .unwrap_or_default();

        let snippet = if let Some(rest) = chunk.split("result__snippet").nth(1) {
            extract_between(rest, ">", "</")
                .map(|s| strip_html_tags(s))
                .unwrap_or_default()
        } else {
            String::new()
        };

        if !title.is_empty() {
            results.push(SearchResult {
                title,
                snippet,
                url,
            });
        }
    }

    results
}

fn extract_between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start_idx = text.find(start)? + start.len();
    let remaining = &text[start_idx..];
    let end_idx = remaining.find(end)?;
    Some(&remaining[..end_idx])
}

fn strip_html_tags(text: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    for ch in text.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

fn decode_ddg_url(url: &str) -> String {
    // DuckDuckGo wraps URLs in redirect: //duckduckgo.com/l/?uddg=ENCODED_URL
    if let Some(encoded) = url.split("uddg=").nth(1) {
        let decoded = encoded.split('&').next().unwrap_or(encoded);
        urlencoding_decode(decoded)
    } else {
        url.to_string()
    }
}

fn urlencoded(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

fn urlencoding_decode(text: &str) -> String {
    let mut result = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                result.push(byte);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).to_string()
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        text.to_string()
    } else {
        format!("{}... (truncated)", &text[..max_chars])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<b>bold</b> text"), "bold text");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("<a href=\"x\">link</a>"), "link");
        assert_eq!(strip_html_tags("&amp; &lt; &gt;"), "& < >");
    }

    #[test]
    fn test_extract_between() {
        assert_eq!(extract_between("hello [world] end", "[", "]"), Some("world"));
        assert_eq!(extract_between("no match", "[", "]"), None);
    }

    #[test]
    fn test_urlencoded() {
        assert_eq!(urlencoded("hello world"), "hello+world");
        assert_eq!(urlencoded("rust lang"), "rust+lang");
        assert_eq!(urlencoded("a&b"), "a%26b");
    }

    #[test]
    fn test_urlencoding_decode() {
        assert_eq!(urlencoding_decode("hello%20world"), "hello world");
        assert_eq!(urlencoding_decode("a%26b"), "a&b");
        assert_eq!(urlencoding_decode("plain"), "plain");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 100), "short");
        assert_eq!(truncate("hello world", 5), "hello... (truncated)");
    }

    #[test]
    fn test_decode_ddg_url() {
        let ddg = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com&rut=abc";
        assert_eq!(decode_ddg_url(ddg), "https://example.com");
    }

    #[tokio::test]
    async fn test_register_tools() {
        let mut registry = ToolRegistry::new();
        register(&mut registry, 5000);
        assert!(registry.has("web_search"));
        assert!(registry.has("web_fetch"));
        assert!(registry.has("api_request"));
    }

    #[tokio::test]
    async fn test_api_request_missing_url() {
        let client = Client::new();
        let result = api_request(&client, json!({}), 5000).await;
        assert!(result.contains("'url' parameter required"));
    }

    #[tokio::test]
    async fn test_web_fetch_missing_url() {
        let client = Client::new();
        let result = web_fetch(&client, json!({}), 5000).await;
        assert!(result.contains("'url' parameter required"));
    }

    #[tokio::test]
    async fn test_web_search_missing_query() {
        let client = Client::new();
        let result = web_search(&client, json!({}), 5000).await;
        assert!(result.contains("'query' parameter required"));
    }
}
