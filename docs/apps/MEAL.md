# meal — a photo of a meal in, items and calories out

`POST /estimate` takes a photo and returns what is on the plate, with grams, calories and macros,
a confidence, and what to double-check. It is three components, composed by
`cargo xtask compose meal`:

| component | what it is |
|---|---|
| `meal-domain` | the HTTP front: routing, media-type sniffing, the text and JSON forms, the error statuses |
| `meal-estimate` | `nutrition:meal`: owns the **prompt** and the **parser**, does the sums, flags what does not add up |
| `deepseek-vision` | `vision:describe` over DeepSeek's Chat Completions API (`deepseek-chat`) |

Nothing here is a script or a native binary (ADR-0095): a vision call is an HTTPS POST.

```
POST /estimate?note=post-row%20lunch[&format=json][&media-type=image/jpeg]
     body: the image bytes, Content-Type: image/jpeg | image/png | image/webp | image/gif
200  text/plain (or application/json with ?format=json)
415  the body is a different type than it was sent as, or no supported type was given
422  the model looked and said nothing
502  the vision provider refused this deployment, or its answer was not an estimate
503  the vision provider is unavailable
```

Plain text is the default because the first caller is an agent that reads it whole. An estimate from a
photo is a guess about portions and the output says so; `check:` is where the model's own doubts
and the parser's (calories that disagree with the macros, a portion no plate carries) are listed.

## Configuration

`[config]` (not secret: config is visible to every app on a host)

- `deepseek-model` — default `deepseek-chat`. DeepSeek's reasoning models spend a small token budget
  thinking and return empty text, which comes back as 422.
- `deepseek-base-url` — default `https://api.deepseek.com`. Point it at a compatible server to run
  without a key.

## The key

The provider reveals the secret `deepseek-api-key` from the vault (`comp:secrets/reader`, ADR-0051),
and **a host with no platform has no vault**: `comp-host`'s `--platform-url` is empty in single-host
mode, so the component is told its secret is unavailable and the app answers 502 naming it. Either
run the app where a platform serves secrets, or set `deepseek-base-url` to something that holds the
key. This was verified both ways, against a stand-in model and against the real DeepSeek through a
forwarder that adds the key (so it never enters the component).

## Checked

A plate of roasted chicken thigh, rice, roasted potatoes, cauliflower, broccoli and caramelised
onion with almonds came back as six items, ~800 kcal, `confidence: medium`, with the model's own
doubts about cooking fat and the hidden rice. The same photo mislabelled as PNG is a 415, not a guess.
