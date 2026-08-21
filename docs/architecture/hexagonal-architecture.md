# Hexagonal architecture for xmusic

Date: 2026-08-21
Status: applied 2026-08-21

## What was built

All seven migration steps below are done. The layout that resulted differs from
the sketch further down in three places, and the built version is the one that is
true:

- There is no separate `actor.rs`. `PlayerService` is the single owner of the
  daemon's mutable state and serialises its own transitions behind its mutexes,
  which is what the actor was for. Adding a thread and a channel in front of it
  would have added a hop without removing an owner.
- `SearchState`/`SearchResult` are `ListState`/`ListItem` in the protocol crate.
  A search, a library feed, and a playlist's tracks are one shape with different
  origins, and `Source` names the origin, so nothing is called a search that is
  not one.
- `PageLifecycle` models only what the daemon controls — `Live`, `Hibernating`,
  `Waking`, each carrying the resume point where it has one. Whether the page has
  finished booting stays where it is reported from, in `PlayerState`. Two owners
  of one fact is exactly how a page ends up described as ready and hibernating at
  once, which the enum was meant to prevent.

`tui/src/panes.rs` is part of the model rather than a fourth file in the Elm
loop; it holds per-pane list state and the drill-down stack, and knows nothing
about keys, HTTP or drawing.

## Summary

xmusic should keep its existing two-process design and use a lightweight
hexagonal architecture inside each process.

- `xmusic` owns terminal interaction and daemon management.
- `xmusic-player` owns the hidden YouTube Music page and audio playback.
- A shared protocol crate should define the JSON messages exchanged between
  them.
- Application behavior should not depend directly on HTTP, Tauri, Ratatui,
  browser-cookie storage, or JavaScript.

This is not a proposal for microservices or a large framework. It is a way to
keep the small codebase understandable as library browsing, playlists, queue
management, and more interfaces are added.

## What hexagonal architecture means

Hexagonal architecture separates application behavior from the technology used
to reach it.

Think of the application as a device:

- The **core** contains the behavior and rules.
- **Ports** describe what the core can do or what it needs from the outside.
- **Adapters** connect those ports to concrete technology.

The word "hexagonal" comes from diagrams commonly used to show several ports
around an application. It does not require six modules or a literal hexagon.

For xmusic, the player core should understand operations such as:

- Search for music.
- Play a selected track.
- Pause, resume, skip, seek, and change volume.
- Wake an unloaded page before an operation that needs it.
- Hibernate an idle page and remember a resume point.

It should not need to understand:

- HTTP headers or status codes.
- `tiny_http` request objects.
- Tauri's `AppHandle` or `WebviewWindow`.
- JavaScript evaluation details.
- Ratatui widgets or terminal key events.
- Browser cookie database locations.

## Ports and adapters in xmusic

An inbound adapter translates an outside request into an application operation.
An outbound adapter implements something the application needs from an external
system.

```text
Terminal events
      |
      v
TUI model -> update -> effects -> HTTP client adapter
                                      |
                                Local JSON API
                                      |
                                      v
HTTP server adapter -> player service/actor -> page port
                                               |
                                               v
                                      Tauri/webview adapter
                                               |
                                               v
                                      YouTube Music + inject.js
```

Examples:

| Direction | Port or operation | Adapter |
|---|---|---|
| Inbound | `search(query)` | Local HTTP route `POST /search` |
| Inbound | `play(video_id)` | Local HTTP route `POST /play` |
| Inbound | Page state report | Tauri IPC command |
| Inbound | Idle sweep | Timer thread |
| Outbound | Dispatch a page command | Tauri webview and `inject.js` |
| Outbound | Navigate or show the page | Tauri window adapter |
| Outbound | Read daemon credentials | Runtime-file adapter |
| Outbound | Import a browser session | Browser-cookie adapter |

The HTTP adapter should authenticate, deserialize, and map errors to HTTP
responses. It should then call an application service rather than implementing
playback and lifecycle behavior itself.

For example:

```text
POST /play
    |
    v
Validate PlayRequest
    |
    v
PlayerService.play(video_id)
    |
    +-- ensure the page is ready
    +-- dispatch the play command
    +-- wait for the acknowledged result
    |
    v
Map PlayerError to an HTTP response
```

## Why this fits the codebase

The current process boundary is already correct. The terminal does not need to
know how YouTube Music works, and the player daemon does not need to know how the
terminal is drawn.

The main architectural pressure is now inside those processes:

1. Protocol models and constants are duplicated between the player and TUI.
   They can silently diverge when a field or protocol rule changes.
2. `player/src/server.rs` handles authentication, JSON parsing, validation,
   wake-up policy, application orchestration, Tauri calls, and HTTP error
   mapping.
3. Hibernation and HTTP handling depend on each other. Page lifecycle should be
   owned by the player application rather than by either adapter.
4. Player state is updated by HTTP requests, Tauri IPC reports, and timer
   threads. A single state owner would make these transitions easier to reason
   about.
5. The TUI is already close to a unidirectional design, but its application
   state owns a concrete client and performs effects directly.

The YouTube Music page is also the most fragile dependency in the project.
Keeping it behind a page port means a page change is contained in the Tauri and
JavaScript adapter instead of spreading through the HTTP server and TUI.

## Recommended player design

Use one application service or actor as the owner of player behavior and mutable
state.

```rust
enum PlayerCommand {
    Search { query: String },
    Play { video_id: String },
    Transport(TransportAction),
    Seek(SeekChange),
    SetVolume(VolumeChange),
    ShowWindow,
    HideWindow,
    ImportCookies(Vec<Cookie>),
}

enum TransportAction {
    Play,
    Pause,
    PlayPause,
    Next,
    Previous,
}
```

HTTP requests, Tauri reports, and idle timer events should send messages to this
single owner. It can serialize state transitions and publish read-only snapshots
for `GET /state` and `GET /search-results`.

The page connection should be represented by a small port. Exact methods may
change during implementation, but the dependency direction should look like:

```rust
trait PageDriver {
    fn navigate(&self, destination: PageDestination) -> Result<(), PageError>;
    fn dispatch(
        &self,
        command: PageCommand,
        timeout: Duration,
    ) -> Result<(), PageError>;
    fn set_visible(&self, visible: bool) -> Result<(), PageError>;
}
```

Only the Tauri adapter should implement this trait using `AppHandle`, webview
navigation, JavaScript evaluation, and the command reply bridge.

### Player lifecycle

Page lifecycle should be modeled explicitly instead of inferred from several
independent flags.

```rust
enum PageLifecycle {
    Loading,
    Ready,
    Hibernating { resume: Option<ResumePoint> },
    Waking,
    Failed { diagnostic: String },
}
```

This makes invalid combinations harder to represent. For example, the page
should not appear both ready and hibernating.

The application service should decide which capability an operation needs:

- Search and cookie import need the YouTube Music API configuration.
- Playback controls need the actual player element.
- State reads can use the latest cached snapshot without waking the page.

The HTTP adapter should not contain this policy.

## Recommended TUI design

Use a small Elm-style unidirectional loop:

```text
Input or client event
        |
        v
      Message
        |
        v
update(Model, Message) -> Effects
        |                  |
        v                  v
   New model          Effect runner
        |
        v
    view(Model)
```

The pieces are:

- **Model:** Current player, search, selection, input, status, and UI mode.
- **Message:** Key press, tick, player update, search update, or failure.
- **Update:** Pure state transition that returns effects to perform.
- **Effect:** Search, play, seek, import cookies, or stop the daemon.
- **Effect runner:** Existing worker-thread behavior and concrete adapters.
- **View:** Ratatui rendering from the model.

This preserves optimistic UI updates while keeping HTTP and process operations
outside the model.

## Shared protocol crate

Add a small workspace crate used by both binaries:

```text
protocol/
  Cargo.toml
  src/
    lib.rs
    model.rs
    request.rs
    action.rs
    error.rs
```

It should contain only wire-level definitions and stable constants:

- `PlayerState`
- `SearchResult` and `SearchState`
- `HealthResponse`
- Typed request bodies
- `TransportAction`
- Structured error response
- Protocol version
- Loopback address and authentication-header name, if these remain fixed

Both binaries should depend on this crate. The protocol crate must not depend on
Tauri, Ratatui, `tiny_http`, or `ureq`.

```text
xmusic-protocol <- xmusic-player
xmusic-protocol <- xmusic-tui
```

The player and TUI should not depend on each other.

## What DDD means

DDD means Domain-Driven Design. It organizes software around the language and
rules of the problem rather than around frameworks or storage technology.

Possible xmusic domain terms include:

- `Track`
- `PlaybackState`
- `TransportAction`
- `SearchQuery`
- `ResumePoint`
- `PageLifecycle`

DDD asks questions such as:

- What states can the player enter?
- What must be true before a track can play?
- What should happen when a hibernating page receives a search?
- Which state transitions are valid?
- Which terms should the code, API, and documentation use consistently?

DDD and hexagonal architecture solve different problems:

- **DDD** helps model and name the problem correctly.
- **Hexagonal architecture** prevents technical integrations from controlling
  that model.

They can be used together, but xmusic does not need full DDD infrastructure.
Aggregates, repositories, bounded contexts, and a large domain layer would add
more structure than the current problem requires.

The recommendation is lightweight domain modeling: use clear types and explicit
state transitions where they prevent bugs, while keeping the implementation
small.

## Proposed source layout

The exact file count can remain modest. A possible target is:

```text
protocol/src/
  lib.rs
  model.rs
  request.rs

player/src/
  main.rs                 # composition root
  application.rs          # commands and orchestration
  actor.rs                # single state owner
  lifecycle.rs            # wake, hibernate, restore
  ports.rs                # page-facing interfaces
  adapters/
    http.rs
    tauri_page.rs
    runtime_files.rs
  inject.js

tui/src/
  main.rs                 # terminal setup and composition
  model.rs
  update.rs
  view.rs
  effects.rs
  adapters/
    http_client.rs
    daemon_process.rs
    browser_session.rs
```

Do not split every small type into its own file. The important rule is the
dependency direction, not the number of directories.

## Dependency rules

1. Application code may depend on protocol and domain types.
2. Adapters may depend on application ports and external libraries.
3. Application code must not depend on HTTP request types, Tauri handles, or
   Ratatui widgets.
4. The HTTP server translates requests and responses; it does not own playback
   or page-lifecycle policy.
5. The Tauri adapter owns webview mechanics; it does not own search sequencing
   or hibernation decisions.
6. `inject.js` remains the anti-corruption layer around YouTube Music internals.
7. The TUI view reads the model but does not call clients or manage processes.

## Incremental migration

The architecture can be adopted without a rewrite.

1. Add the shared protocol crate and move duplicated models and constants into
   it.
2. Replace string transport actions and untyped JSON bodies with enums and
   request structures.
3. Extract a `PlayerService` from `player/src/server.rs`. Keep authentication,
   request parsing, and HTTP response mapping in the server adapter.
4. Put wake, hibernate, restore, search sequencing, and command acknowledgement
   behind the player service.
5. Introduce a `PageDriver` port and move Tauri-specific operations into its
   adapter.
6. Change the TUI to produce effects from state updates instead of storing a
   concrete HTTP client inside its model.
7. Add browse, library, playlist, and queue features as complete vertical slices
   through protocol, player application, and TUI.

If the product remains limited to search and playback, stopping after the first
three steps is reasonable. Those steps remove the largest duplication and
coupling without adding unnecessary machinery.

## Non-goals

- Splitting the application into more operating-system processes.
- Introducing network-accessible microservices.
- Adding a database without a persistence requirement.
- Implementing event sourcing.
- Building a generic plugin framework.
- Applying every DDD pattern.

The goal is a small core with clear boundaries, not architecture for its own
sake.
