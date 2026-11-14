# Security

## Reporting a vulnerability

Please report a vulnerability **privately**, not in a public issue.

Use GitHub's private reporting: on the repository page open **Security → Report a vulnerability**. Say what you found, how to reproduce it and which version of ahem and which system you used. Please do not include your own API keys, recordings or private texts.

You can expect an answer within a few days. ahem is developed by one person in their free time, so a fix may take longer; I will tell you what I found and when to expect it, and credit you in the release notes if you wish.

## What matters most here

- **API keys** are stored encrypted on the computer (DPAPI on Windows, a key in the login Keychain on macOS) and must never reach a log, a file in clear text or any server but the service they belong to.
- **Dictated text** is not logged by default; audio and text leave the computer only for a service the user has chosen.
- The app talks to the network only for: the services the user connected, downloads of models and engines from their official sources (pinned size and SHA-256), the ChatGPT sign-in if the user uses it, and, when automatic updates are on, this project's releases.

## Supported versions

The latest release. Older ones are not patched.
