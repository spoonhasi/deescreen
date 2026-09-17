# deescreen

Capture and click **one Windows GUI window** on a remote PC, over HTTP. One exe, no installer.

Built so an AI agent could work the operator panel of a CNC simulator — Mitsubishi NC Trainer2
plus, FANUC NCGuide, and the like — but nothing about any of those applications lives in the
code. It all lives in a **profile file**: write down a window title and some rectangles, and
the same exe attaches to any Win32/WinUI application. Two vendors, one exe, no code change —
a CNC panel is just what it was proved against.

**It is meant for remote control of simulators and test machines.** That premise is what the
design is built on, and the warning below is that premise written down, not decoration.

---

## ⚠️ Do not put this on a real equipment HMI

**Driving a simulator and driving a machine are not the same activity.**
This tool can press cycle start and emergency stop. Install it on simulators and test
machines only.

This is not a boundary code can enforce — the simulator's [CYCLE START] and the machine's
[CYCLE START] are the same pixels as far as this tool is concerned. Where you install it
*is* the decision.

The ceiling on what it can do is set by the **profile file**
(`profiles/deescreen.<name>.json`). Which means: **whoever can edit that file holds the
control authority.** Manage file permissions and the IP whitelist on that basis.

---

## Vocabulary

**The thing you press is a "button"** — on screen, in the file, and in the API.

Three of a profile's collections are things a request names — a request picks **one** out of
one of them, so the plural is the file and the singular is the request. Next to each singular
sits the unnamed way of saying the same thing, which is the one a policy flag governs:

| the profile holds | a request names one | or says it unnamed (gated) |
|---|---|---|
| `buttons` | `button` | `rect` / `point` — `allow_raw_clicks` |
| `keys` | `key` | `chord` — `allow_raw_keys` |
| `regions` | `capture=NAME` | `rect` |

The unnamed column is deliberately a **different word** every time, never a variant spelling of
the named one — the gated thing must not be reachable by mistyping the safe thing. `key` looks
up a definition; `chord` bypasses the definitions entirely. Those are opposite acts and they do
not get near-identical names.

Two things sit outside this table. `spell` is on the safe side of it — it turns a string into
a sequence of **named** buttons using the legends in the profile, so it reaches nothing the
first row does not. A **menu path** is the exception: the menu is read off the window rather
than written in the profile, so it has no "the profile holds" column at all, and it has its own
switch (`allow_menus`) for exactly that reason.

`click_button` is the one name in the API that is in no column, because it answers a different
question — not *what* to press but *which mouse button presses it* (`left`, `right` or
`middle`, one per click). It is not called `button` because then one word would mean two things
inside a single object: an entry in `buttons` that itself carries a `button` field makes the
reader stop every time.

## The spine of the design

### 1. The request does not choose coordinates

A request sends a **name**.

```
POST /click  {"button": "cycle_start"}
```

Coordinates live only in the profile file. Let the caller compute raw pixels and it will
**quietly press the wrong thing** — no error, so it takes a long time to notice. A coordinate
that is not on the list cannot be pressed (until you explicitly open that with
`allow_raw_clicks: true`). Key input is treated at the same level.

One thing is outside this, and the word *coordinate* should not be read as a way around saying
so: a **menu item** has no stable rectangle to write down, so `POST /menu` names a path read
off the window instead. That endpoint reaches whatever the application's menu reaches, which is
why it is off unless `allow_menus` is set.

### 2. Coordinates are physical pixels in the window's client area

Moving the window does not break them. Changing the window **size** shifts every coordinate
at once, so `reference_client` records "these were measured at this size" and a mismatch is
**refused**. One `POST /window/fit` puts it back.

Refusing is the default because it is the only answer that is right for every application. A
profile can choose otherwise with `on_size_mismatch`: `"scale"` where the panel really does
stretch with its window, `"ignore"` where the window grows but the panel stays pinned to the
top-left. Both are claims about that specific UI, which is why neither is the default —
guessing wrong presses the wrong pixel and says nothing.

Captures use the same coordinate system — **a pixel measured in a PNG can be written straight
into the profile file.**

### 3. The screen is a last resort

Read from an API whatever an API can tell you. In this project deemesh-hub serves machine
state over HTTP, so the screen is used only to *act* and to see *what exists only on screen*.
That removes any need for OCR and makes the verification loop **"act on the screen, confirm
through the API"**, which is far more robust.

```
1. POST /click {"button":"emergency_stop","confirm":true}   ← deescreen
2. GET  /machine/channel/executionStatus?machine=3          ← deemesh-hub
3. GET  /machine/channel/emergencyStatus?machine=3          ← deemesh-hub
4. POST /click {"button":"emergency_stop","confirm":true}   ← release
5. read again to confirm it went back
```

---

## Quick start

```bash
cargo build --release
```

Copy the single `target/release/deescreen.exe` into any folder on the target PC and run it.
The first run creates `config.json`, `profiles/`, `captures/` and `logs/` next to the exe —
unless the exe sits in a shared bin directory, which is the one case below.

Or, if you have a Rust toolchain on the target PC, one line:

```bash
cargo install --git https://github.com/spoonhasi/deescreen
```

### Where the runtime files go

The exe needs a home for `config.json`, `profiles/`, `captures/` and `logs/`. It picks one at
startup, in this order, and **says which it picked** — in the startup log, in `GET /health`
under `home`, and by opening it from the tray's **Open settings folder**:

| | home | |
|---|---|---|
| 1 | `DEESCREEN_HOME` | set the variable and everything lives there. An explicit answer beats the rest |
| 2 | beside the exe | a `config.json` is already there, so this install is portable and stays portable |
| 3 | `%LOCALAPPDATA%\deescreen` | the exe is in a shared bin directory — what `cargo install` does |
| 4 | beside the exe | the ordinary case: the exe was dropped in a folder, and that folder is the install |

Rules 2 and 4 are the behaviour this has always had, so an existing install does not move. Rule
3 exists only because `cargo install` puts the exe in `~/.cargo/bin`, where creating four
runtime entries would scatter them among every other tool installed the same way.

The check for rule 3 is deliberately narrow — `CARGO_HOME/bin`, or a path ending in
`.cargo/bin`. A folder of your own that merely ends in `bin` is not one, because a wrong guess
here moves somebody's config file.

> **The two `*.example.json` files are documentation, not templates — do not copy them over
> what the first run generated.** They exist so you can read the full shape of a config and of
> a profile without starting the program, and the test suite parses both, so neither can drift
> from what this build actually accepts. But `config.example.json` shows the *LAN* case:
> `0.0.0.0` plus a second machine on the whitelist. Copy it onto a generated `config.json` and
> you have replaced the localhost-only default with an open port and a whitelist entry pointing
> at a machine that is not yours. The addresses in it are `192.0.2.x` — reserved for
> documentation, so they match no real host — which is exactly why they must be replaced rather
> than kept. Edit the generated file; read the example.

`profiles/` starts **empty** — profiles are made in the editor, and until one exists there is
no window to capture and no button to press. `/health` reports that as a problem, and the
editor's first screen spells out the steps.

**The default allows `127.0.0.1` only.** That is deliberate: copy the exe onto someone else's
PC and no controllable port opens on the LAN without a decision. To use it from a development
PC, put that IP in `allowed_ips_read` / `allowed_ips_write` in `config.json`, set `host` to
`0.0.0.0`, and restart. (`config.json` is read at startup only. The profile files — the part
you actually edit often — reload with `POST /admin/reload`.)

### What it looks like when running

A release build runs **in the tray with no console window.** Double-clicking the icon opens
the **button editor** in a browser. Right-click menu:

| item | |
|---|---|
| `deescreen v0.6.4` / `http://127.0.0.1:8090` | display only |
| **Open button editor** | same as double-click |
| **Status (/health)** | is it in a state where it can act |
| **Open settings folder** | the home directory — where `config.json`, `profiles/` and `logs/` actually are |
| **Quit** | clean shutdown |

If startup fails (a typo in the config, a port collision), there is no console to print to,
so it **puts the reason in a message box.** The details go into today's file under `logs/`.
A debug build (`cargo build`) keeps the console — during development it is better to see the
log immediately.

> The tray menu opens `127.0.0.1` on that PC. Drop localhost from the whitelist and those
> links return 403, so the startup log warns when the config is set that way. From a remote
> machine, open `http://192.0.2.73:8090/editor` in your own browser instead (that IP has
> to be in `allowed_ips_read`).

### Reading small text — `scale` goes both ways

`scale<1` shrinks (any capture). **`scale>1` magnifies**, and magnifying is allowed **only on
a crop**:

```bash
curl -s -o keys.png "http://192.0.2.73:8090/capture.png?rect=820,600,300,80&scale=4"
```

The reason for refusing to magnify the whole screen is plain arithmetic — 1920×1080 at 4× is
33 megapixels, which is no use to the reader either. A crop is what bounds the output size,
and that condition is exactly the rule. The caps are **8×** and **4 megapixels**, and
`max_width` bounds the output width in **both** directions, so `&scale=8&max_width=1200`
means "as large as fits inside 1200px".

Magnification uses **nearest-neighbour, no interpolation**. The point is to see the glyphs
larger, not to invent detail, so hard edges beat a smeared resample.

> `mark=x,y&inset=4` also magnifies, but that enlarges the area **around one point**.
> Enlarging "this row of six keys" is `rect=` plus `scale>1`.

### Driving more than one application — profiles

One profile = **one window plus its entire coordinate universe** (buttons, regions, keys,
reference size). A different application puts the same-named button somewhere completely
different, so swapping only the window is not a coherent operation.

**Drop in a file and you have a profile.** The first run creates a `profiles/` folder next to
the exe; each `deescreen.<name>.json` inside it is the profile of that name. No `config.json`
edit needed:

```
C:\Portable\DeeScreen\
  deescreen.exe
  config.json
  logs/
    deescreen-2026-08-26.log   → one per day, last 30 kept
  captures/
  profiles/
    deescreen.ncguide.json     → profile "ncguide"
    deescreen.nctrainer.json   → profile "nctrainer"
```

**[＋ Profile]** in the editor header creates the file and registers it in one step. If you
placed a file by hand, `POST /admin/reload` (with no profile named) rescans the folder and
picks it up **without a restart**.

Put a plain-language line at the top of every profile file saying **what it is**:

```json
"description": "the FANUC simulator that mirrors the real machine's screen",
"window": { "title": "NCGuide" }
```

That line is carried into `/health`. The profile name is a slug (`ncguide`) and the window
title is technical, so when a person says "use the FANUC simulator" this sentence is the only
thing an agent can connect that to. `/help` says explicitly: **match what the person said
against the descriptions and window titles — do not guess.**

Requests select by name:

```bash
curl -s "http://192.0.2.73:8090/help?profile=nctrainer"
curl -s -X POST -o shot.png ".../click.png?profile=nctrainer&button=cycle_start"
```

There is one rule for omitting it: **when it is ambiguous, do not choose.**

| situation | omitting `profile` gives you |
|---|---|
| `default_profile` is set in `config.json` | that one |
| not set, and there is exactly **one** profile | that one |
| not set, and there are **two or more** | 404, with the known names listed |

It never quietly takes the first in the list. If it did, adding one profile could re-aim every
call that left the name out, just by changing alphabetical order — a structure where name
ordering decides what gets pressed is precisely what this design is built to avoid. If you run
several and do not want to name one every time, write one line in `default_profile`; that line
*is* the declaration.

An unknown name is refused the same way — **nothing is picked on your behalf** — with the
known list attached.

The `profiles` map in `config.json` layers on top of discovery — you only need it for files
that break the naming rule or live in another folder.

> **One broken file does not stop the rest.** A profile that fails to parse is skipped loudly
> and the others come up — if one bad file out of three took the other two down, you could not
> even get into the editor to fix it.
>
> **Input is serialised across all profiles.** There is one mouse and one foreground on a PC,
> so driving two windows at once would have them stealing focus from each other.

### Every setting in `config.json`

These files are **strict JSON — no comments.** What each setting means lives here and in
`/help`, not beside the value, so a saved file never disagrees with its own documentation.

| setting | |
|---|---|
| `host` | bind address. `127.0.0.1` = this PC only, `0.0.0.0` = every NIC |
| `port` | default 8090. Two instances on one PC need different ports — and therefore different homes, since a home holds one `config.json` |
| `allowed_ips_read` | IPs allowed on the read endpoints |
| `allowed_ips_write` | IPs allowed on the control endpoints. `[]` = observation-only |
| `admin_code` | required on `/admin/*` in the `X-Admin-Code` header. Empty disables it |
| `profiles` | an **extra** name → file map, layered on what was found in `profiles/`. Only for files that break the naming rule or live elsewhere. Relative paths resolve inside the home directory |
| `default_profile` | which profile a request gets when it omits `profile`. Empty = only works while exactly one exists |
| `captures.dir` | where capture PNGs accumulate. Relative resolves inside the home directory; absolute is taken as written |
| `captures.keep` | how many recent captures to keep; the oldest beyond this are deleted |
| `captures.max_age_minutes` | delete captures older than this regardless of count. `0` = no age limit |
| `allow_raw_clicks` | whether unnamed coordinates may be clicked. **This is the boundary in §1 of the design** — default `false` |
| `allow_raw_keys` | whether unnamed key input (`chord`/`text`) is allowed. Default `false`; a key is as powerful as a click on a panel that maps them |
| `allow_menus` | whether the window's own **menu bar** may be invoked (`POST /menu`). Default `false`; a menu is read off the window rather than written in the profile, so it reaches further than the named buttons. `GET /menus` is not gated |
| `allow_profile_editing` | whether `/editor` may write profile files. Default `false`; on, the boundary moves from file permissions to HTTP reachability |
| `default_settle_ms` | default wait between an input and the re-capture. Default 500 — a capture taken early returns the previous screen, which reads as a failed operation |
| `max_settle_ms` | ceiling on the wait a request may ask for, so a connection is not held open. Default 10000 |
| `default_hold_ms` | how long a press stays down. Default 80 — see below |
| `max_hold_ms` | ceiling on the hold a request may ask for. Default 2000; the input lock is held for the whole press |

`config.json` is read **at startup only** — restart after changing it. The profile files are the
part that hot-reloads.

**Upgrading over an old install adds the settings it is missing.** Copy a newer exe over an
older one and its `config.json` predates whatever settings the new build brought. Those still
work — they fall back to their defaults — but they are in force without appearing in the file,
so nobody can see there is anything to tune. At startup any absent setting is written in at the
value already being used, and the log names what was added. Existing values are never touched,
and a file that cannot be written is a warning, not a failure. This is safe only because the
config refuses unknown fields: a file that parsed is fully represented, so writing it back
cannot drop anything it had.

### Opening the port on the target PC

```powershell
New-NetFirewallRule -DisplayName "deescreen" -Direction Inbound -Protocol TCP `
  -LocalPort 8090 -RemoteAddress 192.0.2.20 -Action Allow
```

Narrowing to just the development PC with `-RemoteAddress` is worth doing at the firewall as
well as in the whitelist.

---

## How to measure coordinates

**Find the window first.** You almost never know the title, so ask:

```bash
curl -s http://192.0.2.73:8090/windows
```

Pick a `title` and `class` and write them into the profile's `window`. If several windows
match, clicking is **refused**, so narrow it with `class` or `title_exact`.

From there, three routes. **With a person present, the browser is much faster.**

### Route A — draw it in the browser (recommended)

```
http://192.0.2.73:8090/editor
```

> **The editor speaks Korean or English.** The switch is at the right of the header; the choice
> is remembered in that browser, and a first visit follows the browser's own language. Only this
> page is translated — `/help`, `/health` and every API error stay English, because those have to
> line up with the words in the source and in this document.

Work down the sidebar in order:

1. **Target window** — pick from the list and the title and class fill in, and the picture
   switches to that program **before you save** (`POST /preview.png` renders the candidate
   document). It also counts how many windows match right there (several means the server will
   refuse to act, so narrow with class or exact match).

   > A freshly created profile stays **black until you pick.** Showing some arbitrary window
   > would have you drawing rectangles on the wrong program's screen, and that means
   > coordinates belonging to an entirely different window. Better to show nothing.

2. **Reference size** — `[Fit to current window]` records the current client size. Skip it and
   the rectangles you draw are saved against a stale reference, so every one of them is
   refused from the moment you save.
The header also carries **Rename** and **Delete profile** for the profile in the picker. Both
call the same endpoints an agent would, so the same rules apply — a delete sets the file aside
under a timestamped name rather than erasing it, and neither will touch the profile
`config.json` names as `default_profile`, since undoing that needs a config edit and a restart.

3. Drag rectangles on the live capture. Fill in name, note and `confirm` beside it, and nudge
   with `←↑→↓` (1px, Shift for 10px). `Delete` removes the selected one and **selects the
   next**, so you can work through a list without re-picking each time (it does nothing while
   the cursor is in a text field). **Detect controls** finds child controls and draws the
   rectangles for you, where the application supports it (see `/controls` below). Expect far
   more to throw away than to keep.

When you are done, one of two things:

- **Export** — download the finished JSON, a person puts it in `profiles/` and calls
  `/admin/reload`. The permission boundary stays on the file. **This is the default path.**
- **Save** — requires `allow_profile_editing: true`. That moves the boundary to HTTP
  reachability, so turn it on knowingly. `admin_code` can add one more layer (optional).

### Route B — the agent measures, a person checks

For when nobody is present, or the agent has to produce a proposal itself.

1. **Capture with the grid on.** The labels are source coordinates, so they stay readable even
   in a shrunken image.

   ```bash
   curl -s -o shot.png "http://192.0.2.73:8090/capture.png?grid=50"
   ```

2. **Check the guess before pressing.** This adds a crosshair and a magnified inset. No input,
   so zero risk.

   ```bash
   curl -s -o check.png "http://192.0.2.73:8090/capture.png?mark=420,310&inset=4"
   ```

3. **Check the whole proposal in one picture.** Send the candidate definition in the body and
   it is drawn over the current screen and returned — **nothing is saved.** Render-after-save
   would mean it went live before anyone checked it, so the order matters.

   ```bash
   curl -s -X POST --data-binary @proposed.json -o proof.png http://192.0.2.73:8090/preview.png
   ```

   Buttons that fell outside the client area are drawn **hollow** — their own colour, no
   fill — and reported as `outside_client`. A document that fails validation comes back as 422 with the reason —
   filtered out before a person is involved.

4. The agent hands the person the **proposed JSON plus `proof.png`**. They look at one picture
   and commit the file. Writing that file is the approval.

5. **Apply it**

   ```bash
   curl -s -X POST http://192.0.2.73:8090/admin/reload
   ```

   A bad file is not applied — if parsing or validation fails, the old definition stays live.

### Route C — edit it from a program

`GET /admin/profile` returns **exactly the document `POST` accepts.** Read it, change it, send
it back:

```bash
curl -s "http://192.0.2.73:8090/admin/profile?profile=fanuc" > p.json
# edit p.json
curl -s -X POST -H "Content-Type: application/json" \
  --data-binary @p.json "http://192.0.2.73:8090/admin/profile?profile=fanuc"
```

Do not use `GET /profiles` for this — that one is **for reading**, so it flattens the document
into arrays (`buttons: [{name, rect, …}]`). The saved shape is a map keyed by name
(`buttons: {name: {rect, …}}`). Hand-writing a converter works today, but **the day the server
gains a field, that converter drops it silently.** It disappears on the next save and nothing
errors. Send back what you received and there is no converter, and therefore nothing to lose.

> `POST` **replaces the whole document.** Anything you leave out is gone. Send back what you
> read with your edits applied — never a fragment.

#### Changing one thing — `PATCH`

Read-modify-write is the wrong shape for a small edit. The 140-button FANUC profile above is
37 KB, roughly **6,000 tokens** — so adding one button costs that twice, and the writing half
means an agent re-typing 140 rectangles. Move one digit in one of them and it validates, saves,
and answers success. That is the failure this whole tool is built to avoid, produced by the
edit protocol itself.

`PATCH` sends only the difference:

```bash
curl -s -X PATCH -H "Content-Type: application/json" \
  -d '{"buttons": {"NEW_KEY": {"rect": [820,640,60,40]}, "OLD_KEY": null}}' \
  ".../admin/profile?profile=fanuc"
```

It is a **JSON merge patch** ([RFC 7386](https://www.rfc-editor.org/rfc/rfc7386)) and has three
rules:

| in the patch | effect |
|---|---|
| a value | replaces what is at that key |
| an object | **merges** into what is at that key — patching a button's `rect` keeps its `confirm` and `note` |
| `null` | puts the key back the way it was before anyone set it |

Everything not named is untouched. Which makes the two most common edits one line each:

```bash
-d '{"reference_client": [1280,1000]}'
-d '{"regions": {"alarm_bar": [0,940,1280,60]}}'
```

`null` covers unsetting too, which is not obvious about merge patch and is worth stating: the
usual complaint is that it cannot *store* a null. Nothing in a profile is ever stored as a
literal null — an option that is not set is written by leaving the key out — so **removing the
key and clearing the value are the same act**, and one syntax does both:

```bash
-d '{"reference_client": null}'                       # size check goes back to unpinned
-d '{"buttons": {"E_STOP": {"settle_ms": null}}}'     # back to the server default
-d '{"buttons": {"E_STOP": {"point": null}}}'         # back to pressing the rect's centre
-d '{"on_size_mismatch": null}'                       # back to "reject"
```

The one thing to know: **to empty a whole collection send `null`, not `{}`.** An empty object
merges nothing, so `{"buttons": {}}` asks for no change — and the reply says `"changed": false`
rather than pretending it emptied anything.

The reply reports what actually changed, per collection, by name — `added`, `removed`,
`modified`. **A patch that matches what the profile already said writes nothing** and answers
`"changed": false`, so "it worked" and "it was already like that" are never the same answer,
and a good `.bak` is not rotated away for a no-op. A typo inside a patch is refused like any
other unknown field, and nothing is written.

`PATCH` does **not** create profiles — an unknown name is a 404. `POST` is where creating
happens, so a mistyped name cannot quietly become a new empty profile.

**Deleting one button** is just leaving it out of the document you send back — or `null` in a
patch, which is cheaper. Deleting the **profile itself** is a separate path:

```bash
curl -s -X DELETE ".../admin/profile?profile=fanuc&confirm=true"
curl -s -X POST   ".../admin/profile/rename?profile=fanuc&to=nctrainer"
```

- **Delete requires `confirm=true`** — for the same reason a button does. The file is not
  erased; it is moved aside as `deescreen.fanuc.json.deleted-2026-08-26_162651`.
- **Rename is not "save under a new name".** That **copies**: the old one stays, two profiles
  point at one window, and omitting `profile` breaks the moment there are two of them.
  `rename` moves it in place, so neither happens. The old name then 404s with the known list —
  **it is never silently redirected.**
- Neither touches the profile named by `default_profile` in `config.json`. Losing that kills
  every request that omits `profile`, and undoing it takes a config edit plus a restart — a
  person, physically at that PC. One HTTP call should not be able to create a state that
  requires that.

#### What happens to the archive if the same name is deleted twice

**They do not overwrite each other.** The `.bak` a save leaves is one generation deep and the
next save overwrites it, but a delete archive carries **the time in its name**, so every one
is a new file (`_2`, `_3` are appended within the same second). Delete → recreate under the
same name → delete again leaves:

```
deescreen.fanuc.json.deleted-2026-08-26_162651        ← the first one
deescreen.fanuc.json.deleted-2026-08-26_162720        ← the second one
deescreen.fanuc.json.bak.deleted-2026-08-26_162720    ← the second one, one edit earlier
```

The save backup (`.bak`) is moved aside with it. Left in place, it would be overwritten by the
next save of a new profile with that name — **a file that looks like a backup and is not.**

**Nothing ever cleans archives up automatically.** Logs and captures are pruned by count and
age, but this is the last copy of something, so it does not get the same treatment. When they
pile up, a person deletes them.

Two things are blocked at save time:

- **A field name this build does not know stops the whole document.** Both the profile files
  and `config.json` are strict about this. `POST` replaces the entire profile, so a field
  quietly dropped on the way in is a field *saved* as missing — and the one most worth
  mistyping is `confirm`: `confrim` on an emergency stop would store it with no confirmation
  required and answer "saved". Instead the reply names the field and lists what was expected,
  and no file is written. A typo in `config.json` stops startup the same way, with the reason
  in the message box.
- **Two identical names are refused at parse time.** JSON does not forbid duplicate keys and
  the default parser lets the later one win, so without this you send 128 buttons, 127 are
  saved, and **the response says success**. This collision really happens — the MDI letter keys
  `X`/`Y`/`Z` and the operator panel's axis-select `X`/`Y`/`Z`. Making clients send a count to
  cross-check (`expect_buttons=128`) would work too, but that is a discipline every client has
  to remember, and some client will always forget. Blocking it in the parser makes every path
  safe at once — file or HTTP.
- **Names containing characters that get cut in a query string** (`& = # ? % + / \` and
  whitespace) are refused. A name travels as `?button=NAME`, so in a client that forgot to
  encode, such a name is not an error — it is a **different name**. Non-ASCII letters are not
  blocked: forget to encode those and the URL itself breaks loudly, rather than quietly
  becoming something else.

### Three capture region names behave like reserved words

| value | meaning |
|---|---|
| `"client"` | the whole client area |
| `"button:NAME"` | that saved button's own rectangle |
| any other name | that rectangle from `regions` |

Using `@client` **as a region name** is refused — and so is any name starting with `@`,
which is reserved for values the server defines. A region called `@client` would be a ghost
that can never be
selected).

**The request decides what to look at.** Buttons do not carry a default capture region — most
clicks need no confirmation, and a default would capture on every one of them, twice (before
and after, for change detection). Buttons have coordinates, so whoever needs to look can
choose then.

To see whether the saved buttons are still in the right place, add `?buttons=`. Three modes:

| | draws | use when |
|---|---|---|
| `buttons=1` | outline + name + **crosshair** | checking the click point. For a button with an explicit `point`, the click point differs from the rectangle's centre and this mark is the only thing that shows it |
| `buttons=box` | outline + name | **reading the key legends.** The crosshair sits exactly on the click point = the middle of the key = on top of the lettering |
| `buttons=num` | outline + **a number** | buttons packed too tightly for names. The number is the 1-based position in `GET /buttons` (sorted by name), so no legend table is needed |

Labels are placed so they **do not overlap**: four candidate spots are tried — above, below,
inside-top, inside-bottom. If none is free, that one label falls back to its number, and the
metadata's `labels_crowded` says how many did. An overlapping label is not merely ugly, it is
**wrong information** — `MDI_CASE_TOGGLE` and `MDI_Z` running together as `MDI_CASE_TMDI_Z`
gives you no way to tell from the picture whether that is two names or one.

```bash
curl -s -o now.png "http://192.0.2.73:8090/capture.png?buttons=1"
```

`confirm` buttons are drawn red, regions yellow — **colour says what a thing is.** Whether it
can still be reached is the fill: a button whose press point falls outside the client area is
drawn **hollow**, outline only. Two channels rather than one, so a confirm button that has
drifted off the window still reads as a confirm button — which is exactly the fact a single
"something is wrong" colour used to take away. `outside_client` in the metadata names them.

### Checking the names, not the coordinates — the contact sheet

The overlay answers *are these rectangles on the right keys*. It cannot answer *is this the
right **name** for this key*, and the paragraph above is why: sixty names do not fit beside
sixty keys, and the fallback to numbers is the overlay conceding it.

`/sheet.png` is the same rectangles laid out as a **list** instead — one cell per button, its
picture cropped from a single capture, its name underneath with nothing competing for the
space. Read it against the real panel a row at a time.

```bash
curl -s -o sheet.png "http://192.0.2.73:8090/sheet.png?profile=nctrainer-mill"
curl -s -o sheet.png ".../sheet.png?profile=nctrainer-mill&region=operator_panel&scale=2"
```

| | |
|---|---|
| `order=screen` | default — reading order on the panel, so the sheet is a map of it. Rows are worked out from the buttons' own heights, so keys that are not pixel-aligned still form one row |
| `order=name` | the `GET /buttons` order. `MDI_0`…`MDI_9` end up side by side, so the odd picture out shows without knowing the panel |
| `buttons=A,B,C` | only these. An unknown name is a 404 with the near ones, not a quietly shorter sheet |
| `region=NAME` | only the buttons whose **centre** falls inside that region — the usual way to look at a panel of 140 |
| `pad=8` | context pixels around each rectangle (default 8). This is what makes drift visible: at `pad=0` a rectangle sitting 16px off its key still looks like a picture of a key |
| `scale=2` | magnify each crop, for softkeys whose legend is 32px tall |
| `cell=120` | ceiling on one cell's picture, so a whole-panel rectangle does not set the cell size for the other 139 |

A button whose rectangle has no pixels on this window **still gets a cell**, drawn as an empty
crossed box and listed in `not_on_screen`. It is never left out: a name missing from the sheet
is exactly the one nobody checks. If the *anchor* cannot be resolved the sheet is refused
instead — every cell would be empty, and 140 empty cells look like 140 separate problems
rather than the one they are.

`GET /sheet` is the same sheet as JSON — its shape, where it was saved, and that list — for
when the failures are wanted without reading them off a picture.

---

## Using it from an AI — the PNG in one round trip

The tool runs on the simulator PC and the AI is on a development PC, so a server-side disk
path is not something this end can open. Hence the **`.png` variants that hand back the
bytes**.

```bash
curl -s -X POST -o shot.png "http://192.0.2.73:8090/click.png?button=cycle_start&capture=status_bar&settle_ms=500"
```

One line does **click → wait to settle → re-capture**, and `shot.png` lands on the development
PC. Read that file and you have seen it. The metadata (how it was captured, whether it was
black, whether the screen actually changed) rides back in the `X-Deescreen-Meta` response
header.

If you want JSON, `POST /click` does the same thing and gives you the server-side file path
and a `/captures/<name>` URL instead.

---

## API

| method | path | class | |
|---|---|---|---|
| GET | `/` · `/help` | read | **the manual for agents**, one page (see below) |
| GET | `/ping` | exempt | alive or not. Carries no information, hence whitelist-exempt |
| GET | `/health` | read | is it operable right now — checks every trap in **Known traps** below. `status` is `ok` or `degraded`, `problems` lists what is wrong in plain language, `policy` says which gated things are allowed |
| GET | `/windows` | read | visible top-level windows — for finding a title |
| GET | `/window` | read | the configured window's current state (client size, DPI, foreground) |
| GET | `/profiles` | read | **every profile's full definition** — buttons, regions, keys, window. `?profile=` for one |
| GET | `/buttons` | read | one profile's button list (a subset of `/profiles`) |
| GET | `/regions` | read | one profile's region list (coordinates absolute to the window) |
| GET | `/controls` | read | enumerate child controls. **An empty list is an answer** (see below) |
| GET | `/menus` | read | the window's own menu bar — paths, command ids, enabled/checked. Presses nothing |
| GET | `/spell` | read | `?text=G91X0` — which keys that string would press on this keypad. Presses **nothing**; `POST /click {"spell": "…"}` presses it |
| GET | `/editor` | read | the button editor (HTML) |
| GET | `/favicon.ico` · `/favicon.png` | read | the tray icon as a PNG — the tab should not be a different picture from the tray. Two names: the page links the `.png`, a browser asks for the `.ico` on its own |
| GET | `/capture.png` | read | capture as PNG bytes. `?region= &rect=x,y,w,h &pad= &scale= &max_width= &save=`; `region=button:NAME` is that button's own rect <br>overlays: `&grid=50 &mark=x,y &inset=4 &inset_radius=40 &buttons=1\|box\|num` |
| POST | `/capture` | read | same, JSON response (includes the server-side path) |
| GET | `/sheet.png` | read | **one cropped picture per button, with its name under it** — for checking that a name belongs to the key it is on. `?profile= &region= &buttons=A,B &order=screen\|name &pad= &scale= &cell= &save=` |
| GET | `/sheet` | read | the same sheet as JSON: its shape, where it was saved, and which buttons had no picture to show |
| POST | `/preview.png` | read | draw a **candidate** definition from the body over the live screen. Saves nothing |
| GET | `/captures/{name}` | read | fetch a stored capture |
| POST | `/click` | **control** | `{button\|buttons[]\|rect\|point, confirm, click_button, double, spell, settle_ms, measure, quiet_ms, per_press, gap_ms, capture, pad, ignore, scale, max_width}`. No `capture` means **no picture is taken**. `buttons` presses in order and stops at the first failure |
| POST | `/click.png` | **control** | same, PNG bytes back. Parameters go in the query. No `capture` captures the whole client area (an image has to come back) |
| POST | `/menu` | **control**+flag | `{path, confirm, capture, …}` — pick one item from the menu bar. The **only** name here that does not come from the profile, so it needs `allow_menus` |
| POST | `/key` | **control** | `{key\|chord\|text, settle_ms, measure, quiet_ms, capture, pad, ignore, …}` |
| POST | `/window/focus` | **control** | bring the window forward (restore if minimised) |
| POST | `/window/fit` | **control** | restore the client area to `reference_client` |
| POST | `/admin/reload` | read+code | with `?profile=` re-reads that one; without, **rescans the disk** and picks up new files |
| GET | `/admin/profile` | **control** | the profile document, **exactly as POST takes it**. For round-trip editing |
| POST | `/admin/profile` | **control**+flag | replace that document. An unknown `?profile=` name **creates** it. Needs `allow_profile_editing` |
| PATCH | `/admin/profile` | **control**+flag | change part of it — a JSON merge patch (RFC 7386). Only what you name is touched; `null` removes a key. Does **not** create |
| DELETE | `/admin/profile` | **control**+flag | delete the profile. `&confirm=true` required. The file is **moved aside** under a timestamped name |
| POST | `/admin/profile/rename` | **control**+flag | `?profile=OLD&to=NEW`. Moves it in place (not a copy) |
| POST | `/admin/profile/refit` | **control**+flag | re-seat every coordinate onto the window as it is now, for a container that **changed size**. A proposal unless `?apply=true`, and it checks each moved button against a real control first |

Every window-facing endpoint takes **`?profile=NAME`** (or `"profile"` in the body). Omitting
it uses the default profile. An unknown name is refused with 404 and the known list — nothing
gets pressed while it is unclear which window is being driven.

**Classification is by path, not by method** — `/capture` is a POST only because it takes a
body, and splitting by method would file that under control.

Note that `/admin/profile` is **control** class. It is not pressing something now; it is
rewriting *what can be pressed from now on*, which is a stronger authority than a click.

### Pointing another agent at it — `/help`

Give it one address.

```bash
curl -s http://192.0.2.73:8090/help
```

**`/help` is pure manual — it carries no server state.** What the coordinate system is, how to
press things, which traps fail silently; none of that changes. The split of duties:

| | |
|---|---|
| `GET /help` | **how** to use it (fixed) |
| `GET /health` | **what exists right now and whether it works** — profiles, window state, policy |
| `GET /profiles` | **every definition** — all profiles' buttons, regions, keys, window spec |
| `GET /buttons` · `/regions` | when only one of those is needed (smaller response) |

`/profiles` alone makes the other two optional — `?profile=NAME` returns just one.

So `/help` ends by saying plainly what to call next. Without that, readers start inventing
endpoint names.

### Coordinates in a cropped picture are relative to the crop

**A region capture's (0,0) is not the window's (0,0).** Read a pixel off a cropped image and
send it straight back as a click and it is off by the crop offset — silently. Two ways to be
right:

**① Turn on the grid (recommended).** The tick labels are **window coordinates** even on a
crop, so the number you read is the number you send:

```bash
curl -s -o shot.png ".../capture.png?region=status_bar&grid=20"
```

**② Convert.** The `X-Deescreen-Meta` header of that same response carries the crop rectangle
and the scale:

```
window_x = rect[0] + png_x / scale
window_y = rect[1] + png_y / scale
```

Capturing `@client` needs no conversion at all — that image *is* the window's coordinate
system.

### Capturing a button, with a margin — `region=button:NAME` and `pad`

**A toggle's state is usually not inside its button.** On this kind of operator panel the
lamp sits just above the key, so checking "did that switch come on" means capturing a
rectangle wider than the button.

Do not read the rect out of `GET /buttons` and add the margin yourself. Name the button:

```bash
curl -s -o shot.png ".../capture.png?region=button:OPT_STOP&pad=25"
curl -s -X POST -o shot.png ".../click.png?button=OPT_STOP&capture=button&pad=25"
```

- `region=button:NAME` is that button's own rectangle, straight from the profile.
- `pad` grows it on every side, in the same client pixels as the numbers in the file. Where
  that runs past the window edge the picture is **cut there, not slid across** — you get the
  margin there was room for on that side and the full margin on the others, so **near an edge
  the button is not in the middle of the image**. A button 22 wide at x=22 with `pad=200` asks
  for `[-178, …, 422, …]` and comes back `[0, …, 244, …]`: 22 of margin on the left because
  that is all there was, the button's 22, then the 200 asked for on the right. Read its
  position from the `rect` in the metadata rather than assuming, and subtract what falls
  outside before predicting a width.
- On a click, **`capture=button` with no name means the button you just pressed** (the last
  one, for a sequence), so the name is not written twice.

The point is not keystrokes saved. The rect is a number the server already holds, and
re-deriving it in the caller is arithmetic done in a second place — which is where things go
quietly wrong.

### Measuring `settle_ms` instead of guessing it — `measure`

`settle_ms` has to be right — too short and you photograph the screen from before the press
and read it as the result — and until now the only way to learn it was to press the key, guess,
look, and guess again. One key took repeated attempts to pin between 800 and 1000ms.

The server is on the same side of the screen. `"measure": true` has it watch: photograph the
region, compare with the shot before it, repeat, and report when the changing stopped.

```bash
curl -s -X POST -H "Content-Type: application/json" \
  -d '{"button":"MONITOR","measure":true}' http://192.0.2.73:8090/click
```

```json
"settle": { "measured": true, "settled": true, "last_change_ms": 850,
            "quiet_for_ms": 310, "samples": 22,
            "resolution_ms": 53, "quiet_ms": 300, "suggest_settle_ms": 1100 }
```

`suggest_settle_ms` is the number to write into the profile — a quarter more than the longest
wait seen, rounded up — so it is measured once, here, rather than guessed by every caller
afterwards:

```bash
curl -s -X PATCH -H "Content-Type: application/json" \
  -d '{"buttons": {"MONITOR": {"settle_ms": 1100}}}' ".../admin/profile?profile=NAME"
```

It **replaces** the fixed wait rather than following one, so the request takes exactly as long
as the watching did and the reply's `settle_ms` is that real number. Naming no capture region
watches the whole client area, since a measurement is a comparison of pictures and needs one.

**"Settled" means "held still for `quiet_ms`" (300 by default), which is a definition and not an
observation.** An application that pauses longer than that between repaints is called settled
during the pause, and nothing outside the process can tell the difference — so raise `quiet_ms`
where a screen is known to arrive in stages.

`"settled": false` means it never held still, and then the numbers are not a settle time at
all: something is animating. `ignore=x,y,w,h` drops that rectangle from the comparison. The
reply says so outright instead of handing back the ceiling as though it were the answer.

Measuring costs a capture every 50ms until the screen is still, which is why it is opt-in. Do
it once per button that needs it and write the number down.

### Pressing several buttons in order — `buttons`

Entering data on an MDI keypad is one press per character; `G91 G28 X0;` is ten of them.
Send the sequence instead:

```bash
curl -s -X POST -H "Content-Type: application/json" -d '{
  "buttons": ["MDI_G","MDI_9","MDI_1","MDI_G","MDI_2","MDI_8","MDI_X","MDI_0",
              "MDI_EOB","MDI_INSERT"],
  "gap_ms": 150, "capture": "hmi_display" }' .../click
```

**This is not mainly about round trips.** A partial string left in the machine is worse than
no string at all — press CYCLE START after it and an unintended block runs. That is what
shapes the design:

- **Every name is resolved and checked before anything is pressed.** A typo in element eight
  costs nothing instead of leaving seven characters in the machine. The 404 says which index.
- **A failed press stops the sequence.** `sequence.failed` carries the index and the button;
  `pressed` lists what did go in, each with its own `hit`. A partial sequence should be read
  as an unfinished entry — look at the screen before doing anything else.
- **How long the key is held down decides whether a panel notices** — `hold_ms`. A press is
  down, wait, up; that wait is what a machine key is read by. Something scans the contact on a
  cycle, and a press that begins and ends between two scans never happened as far as the
  machine is concerned — nothing moves, no alarm, no error, and the reply looks like a
  success because the input really was delivered. Measured on NC Trainer2 plus, sending down
  and up in one batch made CYCLE START do nothing at all; 80 ms made it run the program. Set
  it in the request while finding the number, then put it on the button or in
  `default_hold_ms`, since the scan rate belongs to the application rather than to one key.
- **A `confirm` button inside the array follows the same rule as a single press** — refused
  unless the request carries `"confirm": true`. An array is not a way around it.
- **The wait before the capture is longer for a sequence** — 800 ms, not the server's
  single-press default. The last press is nearly always the commit (INSERT, INPUT, CYCLE
  START), which does more than a keystroke; capture too early and you photograph the screen
  from just before it — the entry still in the input line, indistinguishable from a sequence
  that failed. Same reasoning as `gap_ms`. The reply always states the `settle_ms` it used,
  and the real fix is a measured `settle_ms` on the commit button in the profile — which
  `"measure": true` produces for you rather than leaving you to find it by repetition.
- **`capture`, `ignore` and `settle_ms` apply once, after the last press.** `gap_ms` is the
  spacing between presses.

`gap_ms` defaults to **500**. A panel that drops input when pressed too fast fails silently —
you get a half-typed block and no error — and that is the exact failure this endpoint exists
to prevent, so the default is deliberately unhurried. Lower it once you have measured yours.

`buttons` cannot be combined with `button`, `rect` or `point`, and at most 200 fit in one
request. In the query form (`/click.png`) it is a comma-separated list:
`buttons=MDI_G,MDI_9`.

> `POST /key` with `{"text": "..."}` is the same idea for real keyboard input. Panel keys are
> painted buttons rather than keys, so they cannot go through that path — hence the same
> facility on the click side.

### Which press in a sequence was ignored — `per_press`

A sequence reports the screen after the **last** press, so a key the application quietly
dropped halfway through leaves no trace: the end screen looks like a working one minus a
character nobody counted, and sending the input succeeded, so nothing errored. `CURSOR_RIGHT`
went missing exactly that way.

```bash
curl -s -X POST -H "Content-Type: application/json" -d '{
  "buttons": ["MDI_G","MDI_9","CURSOR_RIGHT","MDI_1"], "per_press": true }' .../click
```

Each entry in `pressed` gains its own `change`, read between that press and the next — so the
change is attributed to the press that caused it, which the end screen cannot do:

```json
{ "index": 2, "button": "CURSOR_RIGHT", "hit": { "…": "…" },
  "change": { "changed": false, "pixels": 0, "bbox": null } }
```

and `sequence.unchanged` lists those names outright.

**A press that changed nothing is not a failed press.** A toggle already in that state, a key
with no legend to repaint, a key ignored in the current mode and a key that never arrived are
the same picture from outside the process — so `change` says what moved and passes no verdict.
It says *where to look*. If that press's `hit` reports a real, enabled control, the press
length is the next thing to change (`hold_ms`).

One capture per press, added to the sequence's own time, which is why it is asked for. With no
capture region named it watches the whole client area; `ignore=x,y,w,h` applies here too, since
a blinking cursor would otherwise make every press look like it did something.

### Spelling a string instead of naming every key — `spell`

Working out that `G91X0` is `MDI_G, MDI_9, MDI_1, MDI_X, MDI_0` is work the server can do. The
one fact it needs — which key carries which character — belongs in the profile:

```json
"MDI_F": { "rect": [1200, 500, 44, 44], "anchor": "keypad",
           "types": "F", "shift_types": "E" }
```

`types` is the legend printed on the key; `shift_types` is the **second** legend, the small one
above it, reached through the shift key. That *E is on the F key* used to live in an English
sentence in `note`, where nothing could read it and nobody could check it.

```bash
curl -s ".../spell?profile=NAME&text=G91X0"          # resolves, presses nothing
curl -s -X POST -H "Content-Type: application/json" \
  -d '{"spell":"G91X0","capture":"hmi_display"}' .../click
```

**It becomes an ordinary sequence**, so everything that governs `buttons` applies unchanged:
every key resolved before anything is pressed, a failed press stopping the rest, `gap_ms`
between them, `per_press` naming the one that did nothing, and a `confirm` button inside still
requiring `"confirm": true`. The reply's `spelled` says what the string turned into — including
the shift presses, which enter nothing and would otherwise look like stray keys.

A character with no key **refuses the whole string**, and names every such character rather than
the first: fixing them one per round trip means pressing keys in between. Nothing is entered,
because a partial entry is worse than none.

#### Shift is declared, never assumed

```json
"shift": { "button": "MDI_SHIFT", "mode": "oneshot" }
```

| mode | |
|---|---|
| `oneshot` | reaches the second legend for **one** key, then falls back by itself |
| `toggle` | stays on until pressed again |

They are not interchangeable, and the wrong one types a different string with no error — on a
one-shot panel a latch model spells `EE` as `E` then `f`. So a profile that records a shifted
legend without declaring this **does not load**. With `toggle`, spelling always turns it back
off before it finishes: a sequence that ended with the latch on would change what the *next*
caller's presses mean, which is the kind of state nobody thinks to check.

Lower case is spelled on the upper-case key — a keypad is upper case, so `g91` works — and the
reply lists what was folded, because what reached the machine is then the key's character rather
than the one you sent.

`GET /buttons` carries `types` and `shift_types`, and `GET /profiles` carries `shift`, so a
caller that would rather build the sequence itself has everything it needs.

### Pressing something that was never saved — `rect` / `point`

Controls that appear only on certain screens cannot be on the named list. For those, read the
capture and **send the rectangle itself**; the centre gets pressed. Nothing is stored.

```bash
curl -s -X POST -H "Content-Type: application/json" \
  -d '{"rect":[820,640,60,40]}' http://192.0.2.73:8090/click

curl -s -X POST -o shot.png ".../click.png?rect=820,640,60,40&capture=@client"
```

`{"point":[850,660]}` works when you do not know the size. It follows the **same rule** as a
named button (centre of the rectangle), so it also previews how it would behave once saved.

`allow_raw_clicks` has to be on, and the log records `CLICK ... target=(rect 820,640,60,40)`
at **WARN** — the fact that an unverified coordinate was pressed should stand out when
skimming.

**Check what you read before pressing.** This touches nothing:

```bash
curl -s -o check.png ".../capture.png?mark=850,660&inset=4"
```

A misread rectangle presses whatever happens to be there, and that is silent.

### `/controls` only works on certain applications

Standard Win32, MFC and WinForms give each button its own window, so they all show up. **WPF
and WinUI have a single window and return nothing**, as do industrial HMI mock-ups that paint
the panel as one bitmap and hit-test it in code. An empty list is not a failure; it is the
answer "this application has to be measured by hand".

Do not trust a non-empty list either — a measurement on Windows 11 Notepad returned 13
entries, every one of them a WinUI layout container or input sink, and not one a button. Draw
it with `/preview.png` and keep only what sits on a real control.

Key names: `a`–`z`, `0`–`9`, `f1`–`f24`, `numpad0`–`numpad9`, `enter` `esc` `space` `tab`
`backspace` `delete` `insert` `home` `end` `pageup` `pagedown` `up` `down` `left` `right`
`add` `subtract` `multiply` `divide` `decimal` `comma` `period` `slash` … modifiers are joined
with `+` (`"ctrl+shift+f5"`).

---

## When the application moves its own layout — anchors

Some programs put their panel in a slightly different place each time they start. Measured on
NC Trainer2 plus: every container moves **16 px sideways** between runs, in both directions,
depending on how it was launched. The window is `1920×997` either way and every control keeps
its size — only the origin differs.

**Nothing else here can see that.** `reference_client` compares the window, which did not
change. `hit` reports a real, enabled control, because there is one. A 44 px key still takes
the press; a 32 px softkey hands it to its neighbour. It mostly works, which is what makes it
worth a mechanism.

Two things address it, and they are independent.

### The reply says when a coordinate has drifted — `aim`

Every press already looks up the control under the point. Comparing that control's rectangle
with the one in the profile is free, needs nothing written in the file, and works whether or
not anchors are in use:

```json
"aim": { "matches": false, "delta": [16, 0], "saved": [58,882,32,18], "found": [74,882,32,18] }
```

Same size in a different place is a **translation**, which is what a moved layout looks like.
A *different* size means the rectangle was drawn by hand around a control rather than copied
from one, so there is nothing to conclude and the field is absent rather than crying wolf.

### The profile can correct it — `anchors`

An anchor names a control that the coordinates around it were measured from:

```json
"anchors": { "screen": { "text": "NC DISPLAY", "rect": [54, 92, 1104, 818] } },
"regions": { "nc_display": { "rect": [54,92,1104,818], "anchor": "screen" } },
"buttons": {
  "SOFTKEY_01": { "rect": [...], "anchor": "screen" },
  "HEADER_TAB": { "rect": [...], "anchor": "@fixed" }
}
```

Before anything is pressed or captured, that control is found on the window as it is now and
everything belonging to it moves by the difference. **The file is never rewritten** — only the
reading of it changes.

**Matched on text and size together.** Not the class: an MFC window carries its module's load
address in it, so it differs every run (`Afx:00D90000:3:…`, then `Afx:00F20000:8:…`). Not text
alone: this panel has two containers called `OPERATION PANEL`, and they are told apart by being
`328×238` and `708×238`. Size is precisely what a translation leaves untouched. And not "the
one nearest to where it used to be" — that uses the possibly-stale rectangle to find the thing
that would prove it stale, and fails hardest exactly when the drift is largest.

**Declaring one anchor makes the whole profile answer.** Every button and region must then name
an anchor or say `"@fixed"`. There is no third state: an element that says nothing stays behind
while its neighbours move, and the ones that still work hide the one that does not. A profile
with no anchors is untouched by any of this and needs none of it.

If an anchor cannot be found — or two controls match its text *and* size — the request is
refused, for the elements belonging to that anchor only. Falling back to the saved numbers is
what the anchor exists to prevent, so it is not offered.

> **Names never start with `@`.** That prefix is reserved for values the server defines:
> `@client` is the whole client area, `@fixed` is an element that does not move. Reserving the
> prefix rather than individual words means the next one costs nobody a rename.

### When the container itself changed size — `POST /admin/profile/refit`

An anchor corrects a **translation**, and that is exactly why it can be trusted: sizes never
change, so the correction cannot be wrong about anything else. A new build whose operator panel
grew from 708×238 to 746×251 is past that. Every rectangle inside it is wrong by an amount that
depends on how far it sits from the container's own origin, and no single offset expresses that
— which is why the alternative was rewriting the profile element by element with a script.

```bash
curl -s -X POST -H "X-Admin-Code: THECODE" ".../admin/profile/refit?profile=NAME"
```

Nothing is written. The reply is the proposal: where each anchor was and is now, how many
buttons and regions would move, and `verify`.

**Read `verify` before applying.** Each moved button's new click point is looked up on the live
window. `landed` only means *a* control is there; `worst_offset` is the number that matters,
because a point half a key off still lands — on the neighbour. More than a few pixels means the
layout did not scale, it re-flowed, and this endpoint is the wrong tool for that application.

`worst_offset` is taken over the `measured` buttons only — those that are their own control, of
about their own size. A key **drawn inside** something larger (softkeys painted onto the CNC
screen, keys on a panel bitmap) lands on the whole container, and its distance from the middle
of a 640×480 screen is not an error. Those go under `inside_container` instead. Measured on
NCGuide 0i: the 12 softkeys went there, and the other 130 keys came out 12px apart at worst —
where counting the softkeys had reported 306px for a button that was placed correctly. Nothing
here can check the ones inside a container; `GET /sheet.png` can, by eye.

```bash
curl -s -X POST -H "X-Admin-Code: THECODE" ".../admin/profile/refit?profile=NAME&apply=true"
```

A save is **refused** while any moved button lands on nothing, and names them; `force=true`
overrides it for keys the application genuinely has no control for. The previous file is kept
as a backup either way, and `reference_client` is set to the window as it is now — left stale,
`on_size_mismatch` would refuse every click against coordinates that are correct.

**An ambiguous anchor is refused, not guessed.** Anchors are found by text *and* size, and a
refit is for when the size changed — so where two controls carry the same text and neither is
still the saved size, nothing is left to tell them apart. Picking the nearest would use the
rectangle that may be stale to decide which control proves it stale. The reply lists the
candidates; name the one you mean:

```bash
curl -s -X POST -H "Content-Type: application/json" -H "X-Admin-Code: THECODE" \\
  -d '{"anchors": {"OPERATION PANEL": [1142,384,746,251]}, "apply": true}' \\
  ".../admin/profile/refit?profile=NAME"
```

Elements marked `"@fixed"` are **not** moved — somebody stated they do not travel with a
container, and a refit does not overrule that. They are listed under `untouched`. A profile with
no anchors is refused: there is no container to re-seat against, and for a window that merely
changed size `POST /window/fit` or `reference_client` is already the answer.

The arithmetic assumes the layout was *scaled*. That is a guess about the application, not a
measurement — which is the whole reason the guess is checked against real controls before it can
be saved, and why `GET /sheet.png` is worth a look afterwards.

### The window's menu bar — the one thing not written in the profile

A menu item has no stable rectangle. It exists only while the menu is open, it moves with the
length of the items above it, and opening a menu in order to click inside it leaves the
application open if the click then fails. So the menu is reached by its own identity: read the
tree, name the path.

```bash
curl -s ".../menus?profile=NAME"
curl -s -X POST -H "Content-Type: application/json" \
  -d '{"path":"Tool/Set Machine Parameters","capture":"@client"}' .../menu
```

`path` is the full path as `GET /menus` prints it, separated by `/`. The `&` that marks the
underlined letter and the accelerator column (`Ctrl+S`) are already stripped there — do not type
them. Matching ignores case. A `submenu` entry is a place, not an action; naming one lists its
children instead of pressing anything.

**This is the one place where a name does not come from the profile.** Everything else in this
tool can only press what a person wrote into the profile file. A menu is read off the window, so
this endpoint reaches whatever the application's menu reaches — which is why it needs
`"allow_menus": true` in `config.json` on top of the control whitelist. `GET /menus` is not
gated: knowing what is there presses nothing, and refusing to say makes the boundary harder to
reason about rather than tighter.

A profile can mark paths that need a second look, and they behave exactly like a `confirm`
button:

```json
"confirm_menus": ["File", "Tool/Set Machine Parameters"]
```

Matched on **whole path segments** — `"File"` covers the whole File menu and does *not* cover
`"Filename Options"`. Those paths refuse unless the request carries `"confirm": true`.

**A disabled item is refused, not attempted.** The command is delivered as `WM_COMMAND`, which
is what an application receives *after* it has decided an item is enabled — so posting a
greyed-out item's command may well be acted on. `enabled` in `GET /menus` is the state the menu
carries right now; an application that greys items out as the menu *opens* reports everything
enabled here, because nothing ever opens it.

**The command is posted, not sent.** A menu item that opens a modal dialog would otherwise hold
the request open for as long as the dialog is on screen. So the reply means the application
received it, not that it did anything — capture to see. And a dialog that opened is a *new*
window: the profile still points at the old one, so `GET /windows` is how you find it.

An empty list is an answer. Plenty of applications have no menu Windows can see, and some draw
their own (a ribbon, a WPF menu, a custom title bar). A drawn menu is pixels, and is reached by
clicking like anything else.

A submenu can also come back **empty** — `"empty": true`, and listed in `empty_submenus`. That is
usually a menu the application fills at the moment it is opened; NCGuide's File menu is one.
Nothing here ever opens a menu, so those items do not exist from this side, and asking for a path
under one is refused with that reason rather than with a list of similar-looking names.

## Known traps — every one of them fails silently, without an error

`GET /health` checks all of these at once. **When input does not work, start here.**

### DPI scaling

If the display scale is not 100% and the process is not per-monitor DPI aware, the OS quietly
converts coordinates and **the capture's pixels and the click's coordinates end up in
different systems**. deescreen calls
`SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` on the first line of `main()` and
reports whether it worked as `dpi_aware` in `/health`.

### UIPI — input is discarded across privilege levels

If the target application runs as administrator and deescreen does not, **`SendInput` returns
success and does nothing.** MSDN states outright that neither the return value nor
`GetLastError` reports UIPI blocking, so detecting it after the fact is impossible. Two things
stand in for that:

- `input.uipi_risk` in `/health` compares both elevation states up front and warns.
- `hit` in the click response tells you **what was under the point you pressed**, independent
  of pixels.

There is one fix — **run deescreen at the same privilege level as the target application.**

### `changed` cannot tell you whether a click worked

`changed` only answers whether pixels moved. But that is **two independent questions**, and
all four combinations really occur:

| | screen changed | screen identical |
|---|---|---|
| **hit a control** | it worked | blank key · toggle already in that state · ignored in this mode — **all normal** |
| **hit nothing** | a clock or animation moved on its own | the coordinate landed on panel background |

Measured on a FANUC NCGuide operator panel (2026-08-26): **21 of 128 keys are blank keys** with
no legend. Nothing happening when you press them is correct, so pixels cannot tell you whether
the press arrived. In the other direction, `MDI_PAGE_DOWN` reported `changed: true` when the
only thing that had moved was **one digit of the on-screen clock**.

So the two questions are answered separately:

**(a) Did it land — `hit`.** Immediately **before** pressing, the control under that coordinate
is looked up through the window API and returned in the response. No pixels involved.

```json
"hit": { "hwnd": 856538, "class": "WindowsForms10...", "text": "", "id": 0,
         "rect": [820, 640, 60, 40], "visible": true, "enabled": true,
         "depth": 3, "is_window_itself": false }
```

- `is_window_itself: true` — no child control sits there. You pressed background.
- `enabled: false` — it arrived and the control ignored it. **A correct no-change.**
- `rect` — the control's real rectangle. When your coordinate is off, the fix is right here.

A capture with `?mark=x,y` carries the same `hit` — you can verify a coordinate **without
pressing anything.**

> This works only where controls are separate windows (Win32, MFC, WinForms). On WPF or a
> single-bitmap HMI, every point reports `is_window_itself: true`. An empty `GET /controls`
> means you are in the latter case.

**(b) How much changed — `change`.** Instead of a boolean, the pixel count and where.

```json
"changed": true,
"change": { "pixels": 214, "fraction": 0.0001, "bbox": [1180, 12, 60, 16] }
```

`bbox` is in **window client coordinates** and is **one rectangle** around every changed
pixel — two changes far apart enclose everything between them. Even so, "one clock digit" and
"half the screen" are distinguishable.

If a clock keeps forcing `changed` true, exclude that spot from the comparison:

```bash
curl -s -X POST -H "Content-Type: application/json" \
  -d '{"button":"cycle_start","capture":"@client","ignore":[1180,8,90,20]}' .../click
```

`ignore` is accepted by `/click`, `/click.png` and `/key` (as `ignore=x,y,w,h` in a query).

With a `hit` present and `input.uipi_risk` false in `/health`, `changed: false` simply means
**a control that does not repaint**, and nothing is wrong.

### The target PC should be unattended while this runs

Input is injected as **real mouse and keyboard events**, into the one input stream that PC has.
There is no separate, invisible cursor: the pointer physically moves to the button and clicks
it, and `POST /window/focus` really does bring the target window to the front. So while
deescreen is driving something, a person using that PC is sharing an input device with it.

What is guarded and what is not:

- **The press itself is pinned to its coordinate.** The button-down carries the coordinate and
  a move flag in the same `SendInput` batch, and Windows never interleaves a batch with the
  user's own input. Nudging the mouse mid-click cannot move where the click lands.
- **The window is brought to the front before every click**, so a click cannot fall through to
  whatever was covering it.
- **Two requests never overlap** — input is serialised across every profile.
- **A person's own clicking is not guarded at all.** Nothing stops someone clicking in the
  target application between two of our presses, and in the middle of a `buttons` sequence that
  means a keypad entry with something else spliced into it.
- **Focus is taken.** If someone is typing in another window when a click arrives, the
  foreground moves out from under them.

None of this is a fault to fix — it is what driving a real GUI means. Treat the simulator PC as
a machine that is being operated, not one someone is also working at. If a person does have to
step in, stop sending requests first; `/health` and `/capture.png` are read-only and stay safe
at any time.

### Locked screen, disconnected RDP session

If the console session is locked or RDP is disconnected, captures come back black and input
does nothing. deescreen detects this and returns `black: true` with the reason. The target PC
needs a **logged-in, unlocked, active console session**. Running it as a service (Session 0)
produces a warning at startup.

### Pop-up menus do not appear in captures

The default capture is `PrintWindow` — it works even when the window is covered, but
**anything drawn in a separate window, like a drop-down menu, is not in it.** To see one,
bring the window forward (`POST /window/focus`) and capture again. If `PrintWindow` comes back
entirely black, deescreen retries with a screen `BitBlt` on its own and reports which one it
used in `method`.

### Typing a string is slow (on purpose)

`{"text": "..."}` makes one `SendInput` call per character with 4ms between them. Batched into
one array, **characters go missing silently** — measured 2026-08-12 on Windows 11 Notepad, 60
characters sent at once arrived as 17. At the 512-character limit the worst case is about two
seconds.

---

## Security boundary

- **The IP whitelist is split into read and control.** Observation clients go on the read list
  only; keep the control list minimal. `allowed_ips_write: []` is observation-only mode.
- **Localhost is not automatically allowed.** If `127.0.0.1` is not on the list, it is blocked
  on the very PC it is installed on (`/ping` is the only exemption). Assume "it is my own PC,
  it will be fine" and the tray's editor link returns 403 — the startup log warns when that is
  the case. The list is compared literally against the caller's **numeric address**, so writing
  `localhost` never matches; that is rejected at startup.
- **Opening the editor and saving from it are different lists.** Opening is read; saving
  (`/admin/profile`) is control. Put localhost on read only and the editor comes up fine and
  [Save] alone returns 403.
- **`/health` is not whitelist-exempt** — it carries window titles and privilege state.
  `/ping` covers the "is it alive" case.
- `POST /admin/reload` can require one more layer via `admin_code` (`X-Admin-Code` header).
- **`confirm` is a deliberation gate, not a permission gate.** Nothing in this server contacts
  a person or waits for one, and an agent is expected to run unattended — resending with
  `"confirm": true` is the whole mechanism and the caller sends it. What the flag buys is that
  the press cannot happen **by accident**: not from a computed rectangle, not swept up in a
  sequence, not as a reflex after a refusal. Only as a second request that names the button on
  purpose. The question it asks the caller is *is this press part of the work I was given*, not
  *is somebody watching*.
- **`confirm` cannot be bypassed by editing it away either.** A save or patch that leaves a
  button without a `confirm` it used to have — the flag cleared, or the button deleted — is
  refused unless the request carries `&confirm=true`, and nothing is written. Otherwise the
  flag would be advisory: refused at `/click`, patch it off, press — and the button stays
  unprotected for everyone afterwards. This also catches the likelier case, which is not
  cunning but transcription — a round-trip `POST` that re-types 140 buttons and drops one
  `true` would otherwise save and answer success. When it is deliberate the reply lists what
  was removed and the server logs it at WARN, so the record survives the session.
- **`confirm` cannot be bypassed with coordinates.** A button marked `confirm: true` needs
  `"confirm": true` in the request — and so does a **raw coordinate that falls inside one**.
  A rectangle computed from a capture can happen to land on the emergency stop, so the flag
  cannot be allowed to stop meaning anything the moment a caller computes coordinates instead
  of using a name. Deliberate presses are unaffected; accidental ones are caught.
- **Saving profile files over HTTP is off by default.** While it is off, the permission boundary
  is "whoever can write the files on that PC". Turning on `allow_profile_editing` moves it to
  "whoever can reach this port". A save keeps the previous file as `.bak`. `admin_code` can add
  one more layer (optional). The editor still works with the flag off — it downloads the JSON
  for a person to place.
- `/captures/{name}` validates the filename format. This server is not a general file server.
- **There is no HTTPS.** This tool assumes an IP-narrowed private network, and under that
  assumption TLS mostly buys self-signed-certificate friction (`curl --insecure`). If the
  boundary ever has to widen, the first question is not TLS but **whether this belongs there at
  all.**

### Versioning

**`0.x` — anything may change.** That is what major version zero means in semver, and it is
the accurate description: this has run on one LAN, against one application, and the request
shape has already moved several times in response to what an agent using it actually needed.
Pinning the API now would be a claim with nothing behind it.

Within `0.x` the **minor** is the breaking position, so a rename or a removed field goes
`0.1 → 0.2` and a fix goes `0.1.0 → 0.1.1`. Breakage is still announced; it just does not need
a `1.0` to be announced. Whether this ever reaches `1.0` depends on the API sitting still
because nobody needs it to change — not on the tool feeling finished.

---

## How this was built

Written with Claude (Claude Code), over a long conversation. The decisions — what it may do,
what it refuses by default, what things are named — were made by a person and argued through;
the code and most of this prose were written by the model.

That history is why the source reads the way it does. Comments here tend to explain *why*
rather than *what*, and a test tends to pin a decision rather than a line, because the
reasoning existed while the thing was being built and belonged in the file rather than in a
chat log nobody will ever open.

None of which substitutes for reading it. This tool presses buttons on a machine you care
about. Read the source before you point it at one, the same as you would for anything else
that can press a button.

---

## Development

```bash
cargo test          # 93 — coordinate math, crop/scale, overlays, key parsing, ACL classification, example schemas
cargo build --release
```

### One drawing, three places

The icon is **drawn in code** (`src/icon.rs`) rather than loaded from a file, and three things
read it: the tray, the page's favicon, and `build.rs`, which turns it into the icon on the exe
itself. A build script cannot call into the crate it is building, so it `include!`s that file —
unusual, and the reason there is one drawing rather than copies that agree until they do not.
That is also why the file carries no `//!` doc comment and depends on nothing but `std`.

Embedding it needs the Windows SDK's `rc.exe`, driven by the `winresource` **build**-dependency
(it does not ship in the binary). Anything linking `windows-sys` already needs that SDK, so this
adds no requirement — and if the resource compiler is missing the build prints a warning and
carries on with the default icon, because failing a build over a picture would be the worse
trade.

Nothing on the Rust side parses `src/editor.html` — it is `include_str!`'d and served as
bytes, so a syntax error in its script passes every check above while the page does nothing at
all. If you edit that file, check it:

```bash
python -c "import re,pathlib;print(re.search(r'<script[^>]*>(.*?)</script>',pathlib.Path('src/editor.html').read_text(encoding='utf-8'),re.S).group(1))" > /tmp/e.js && node --check /tmp/e.js
```

| file | holds |
|---|---|
| `src/win/window.rs` | finding windows, client coordinates, focus, fitting, control enumeration |
| `src/win/capture.rs` | `PrintWindow` → DIB → RGBA, black-frame detection, `BitBlt` fallback |
| `src/win/input.rs` | `SendInput` mouse/keyboard, key name parsing |
| `src/targets.rs` | the profile document (buttons, regions, keys) — **the ceiling on capability** |
| `src/captures.rs` | crop, scale, PNG, retention, before/after diffing |
| `src/draw.rs` | grid, crosshair, button rectangles, magnified inset + a built-in 5x7 font |
| `src/editor.html` | the button editor (embedded in the binary, makes no external requests) |
| `src/acl.rs` | read/control whitelist split, admin code |
| `src/api.rs` | handlers (the JSON surface and the `.png` surface), and the `/help` manual |
| `src/main.rs` | startup order, the single-instance lock, startup diagnostics |
| `src/config.rs` | `config.json`, profile discovery, the JSON-with-comments parser |
| `src/state.rs` | shared state, profile lookup, which profile an omitted name gets |
| `src/router.rs` | routes and middleware order |
| `src/web.rs` | the response and error shape |
| `src/logging.rs` | one log file per day, 30 days kept |
| `src/tray.rs` | the tray icon and menu (drawn in code, no asset files) |
| `src/win/mod.rs` | the Win32 boundary — DPI awareness, elevation, session checks |

### What got pressed is in the log

Clicks and key input are written to that day's log file **immediately after the press** — so
even if settling or re-capturing then fails, the fact that it was pressed is already recorded.

```
CLICK profile=fanuc target=cycle_start button=left double=false client=(850,660) screen=(882,686) settle=500 window="FANUC NCGuide" pid=1234 hit="WindowsForms10..."/"CYCLE START"
KEY profile=fanuc sent=reset (f1) window="FANUC NCGuide" pid=1234
```

`confirm` buttons and unnamed coordinate clicks (`target=(rect …)`) are logged at **WARN** so
they stand out when skimming. If a capture was requested and **the screen did not change**,
that is a WARN too — it may be a toggle already in that state, but UIPI blocking looks
identical, so this line is the clue when tracing it later.

Logs accumulate in `logs/deescreen-YYYY-MM-DD.log` inside the home directory (the tray's
**Open settings folder** goes there) — **one file per day**,
named by **local time** (both the report and the log are spoken about in terms of the clock on
the wall at that PC). When a new file is created, the oldest are removed past 30 so only the
**last 30 days** are kept. The level is set with `RUST_LOG` (`error|warn|info|debug|trace`,
default `info`). Started from the tray, stderr goes nowhere, so this file is the only trace.

---

## License · distribution

**MIT.** Use it, change it, sell it. There is **no obligation to publish source** — you do not
have to open what you build with this, and you do not have to say you used it.

There is exactly one condition: **when you redistribute it**, include the `LICENSE` file (the
copyright notice and the permission notice). Simply *using* it in-house carries no obligation
at all.

### No binaries are distributed

Screen capture plus synthetic input plus an HTTP listener is **functionally the same shape as
remote-access malware**. An unsigned exe in circulation attracts antivirus false positives
easily, and once one sticks it starts getting blocked on the very machines that actually use
this. So the source is published and everyone builds their own:

```bash
cargo build --release
```
