# Safari on a Mac, by hand

The browser tests run in Chromium, Edge, Firefox and WebKit (Safari's engine) with Playwright, but
not in Safari itself. Before a release, someone with a Mac goes through this list in the current
Safari and the one before it, against a stack they can sign in to (`just compose-up`, or a server),
and writes the Safari versions and anything that failed into the release's pull request. It takes
about fifteen minutes. Use the fixture (`delta`) and test tokens only.

Open Safari's Web Inspector (Develop → Show Web Inspector) on the Console before you start: any red
line there is a finding, even when the page looks right.

## Signing in and out

1. Open the stack's address. The Sign in page shows; nothing else is reachable before signing in.
2. Paste a project manager's token and sign in. The Ask page opens, the token field is empty.
3. In the Web Inspector's Storage tab: Cookies hold `__Host-mercury_session` with Secure, HttpOnly and
   SameSite Strict; Local Storage holds at most the look you picked; Session Storage holds no token.
4. Reload the page: you stay signed in. Sign out with Disconnect: the Sign in page shows, and a reload
   keeps you there.

## Asking

5. Ask _what is blocked?_ about Delta. While it runs a single line names the stage; then the answer
   shows with its facts. The answer appears only once, never word by word before it is checked.
6. Open _How Mercury checked this_: each step with its checks; close it again.
7. Ask _show me the export code_: refused, with the reason and a hint, and no answer.
8. Reload the page on the answer's address (`/ask/<id>`): the answer comes back.
9. Start a new conversation, ask again, then click the first conversation in the side panel: it comes
   back with its answer.
10. With two projects on the token, pick _Any project_ and ask _what is in progress?_: the page asks
    which project; pick one and the answer comes from it.

## A slow or broken connection

11. Ask a question and, while it runs, turn Wi-Fi off for ten seconds, then on: the question ends as
    it would have, without asking twice (the Network tab shows one `POST /v1/questions`).
12. Sign the token out on another device or revoke it (`mctl token revoke`): the next action here
    returns to Sign in with a plain message.

## Sign-off (an approver's token)

13. In a second window (another browser profile), sign in as an approver of the project. Approvals
    shows the held answers with the flagged sentences marked.
14. Allow notifications when Approvals offers it. With the Approvals page open, have the project
    manager ask a question whose answer waits for sign-off: within a minute macOS shows one
    notification, "An answer waits for your sign-off.", with nothing of the answer in it.
15. Approve it: the manager's window shows the answer exactly as the approver saw it. Reject another
    with a note: the manager reads the note.

## Keyboard and look

16. With the keyboard only (Tab, Shift-Tab, Enter, Escape; turn on _Press Tab to highlight each item_
    in Safari's settings first): reach the skip link, the question field, Ask, the side panel and the
    fold, each with a visible focus outline.
17. In Settings, switch the look; reload: it stays, and it is the only thing Local Storage holds.

## The audit (an admin's token)

18. The Audit link shows for an admin only. Open it, export one question: a `.jsonl` file downloads
    with that question's rows.
