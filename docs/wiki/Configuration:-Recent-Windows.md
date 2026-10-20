### Overview

<sup>Since: 25.11</sup>

In this section you can configure the recent windows switcher (Alt-Tab).

The tuned build lays the switcher out in centered rows that fill left to right
at full size and wrap onto a new row when one is full. Navigate it
with the arrow keys: <kbd>←</kbd>/<kbd>→</kbd> (and any configured
previous/next binds) move through MRU order, while <kbd>↑</kbd>/<kbd>↓</kbd>
move one row up or down in the grid.

Here is an outline of the available settings and their default values:

```kdl
recent-windows {
    // off
    debounce-ms 750

    open-delay-ms 150

    highlight {
        active-color "#999999ff"
        urgent-color "#ff9999ff"
        // Tuned build: omit border-color to follow active-color.
        // border-color "#999999"
        border-opacity 1.0
        background-color "#000000"
        background-opacity 0.2
        padding 30
        corner-radius 0
    }

    previews {
        max-height 480
        max-scale 0.5
        antialiasing true // Tuned build only.
        gap 16 // Tuned build only.
    }

    // Tuned build only; text and show default by mode (see below).
    label {
        // text "title"
        // show "all"
        font "sans 14px"
        gap 8
    }

    binds {
        Alt+Tab         { next-window; }
        Alt+Shift+Tab   { previous-window; }
        Alt+grave       { next-window     filter="app-id"; }
        Alt+Shift+grave { previous-window filter="app-id"; }

        Mod+Tab         { next-window; }
        Mod+Shift+Tab   { previous-window; }
        Mod+grave       { next-window     filter="app-id"; }
        Mod+Shift+grave { previous-window filter="app-id"; }
    }
}
```

`off` disables the recent windows switcher altogether.

### `debounce-ms`

Delay, in milliseconds, between the window receiving focus and getting "committed" to the recent windows list.

When you want to focus some window, you might end up focusing some unrelated windows on the way:

- with keyboard navigation, the windows between your current one and the target one;
- with [`focus-follows-mouse`](./Configuration:-Input.md#focus-follows-mouse), the windows you happen to cross with the mouse pointer on the way to the target window.

The debounce delay prevents those intermediate windows from polluting the recent windows list.

Note that some actions, like keyboard input into the target window, will skip this delay and commit the window to the list immediately.
This way, the recent windows list stays responsive while not getting polluted too much with unintended windows.

If you want windows to appear in recent windows right away, including intermediate windows, you can reduce the delay or set it to zero:

```kdl
recent-windows {
    // Commit windows to the recent windows list as soon as they're focused,
    // with no debounce delay.
    debounce-ms 0
}
```

### `open-delay-ms`

Delay, in milliseconds, between pressing the Alt-Tab bind and the recent windows switcher visually appearing on screen.

The switcher is delayed by default so that quickly tapping Alt-Tab to switch windows wouldn't cause annoying fullscreen visual changes.

```kdl
recent-windows {
    // Make the switcher appear instantly.
    open-delay-ms 0
}
```

### `highlight`

Controls the highlight behind the focused window preview in the recent windows switcher.

- `active-color`: legacy normal border color, used when `border-color` is omitted.
- `urgent-color`: urgent border color, taking precedence over `border-color`.
- `border-color`: tuned-build normal border color, independent of the fill.
- `border-opacity`: tuned-build border opacity, from 0 (invisible) to 1 (opaque).
- `background-color`: tuned-build fill color, independent of the border (default black).
- `background-opacity`: tuned-build fill opacity, from 0 to 1 (default 0.2).
- `padding`: padding of the highlight around the window preview, in logical pixels.
- `corner-radius`: corner radius of the highlight. In the tuned build the
  container's corners follow it (see [`container`](#container)).
- `border-width`: tuned build only. Width of the border drawn outside the
  highlight, 0 to 64 (default 2).

The four tuned-build settings hot-reload even while the switcher is open. Put
them in the tuned profile, not a config shared with stock Niri. A color's alpha
channel multiplies its separate opacity setting. The background settings affect
the highlight only, not the full-screen blurred backdrop. Unselected urgent fills
retain their additional 0.4 dimming factor.

```kdl
recent-windows {
    highlight {
        border-color "#999999"
        border-opacity 1.0
        background-color "#000000"
        background-opacity 0.5
    }
}
```

```kdl
recent-windows {
    // Round the corners on the highlight.
    highlight {
        corner-radius 14
    }
}
```

### `previews`

Controls the window previews in the switcher.

- `antialiasing`: tuned-build adaptive GPU downsampling, enabled by default.
  It integrates each thumbnail pixel's source footprint with up to 8×8 bilinear
  samples and rounds corners at the final pixel scale. It preserves live surface
  frame callbacks without allocating offscreen window copies. Icons, titles,
  highlight and backdrop are unaffected. This is a bounded filter, not a guarantee
  of alias-free rendering at arbitrarily extreme reductions.
  Set `antialiasing false` in the tuned profile to hot-reload the original linear
  sampling for comparison or lower GPU cost. Stock Niri does not recognize this key.
- `max-scale`: maximum scale of the window previews.
Windows cannot be scaled bigger than this value.
- `max-height`: maximum height of the window previews.
Further limits the size of the previews in order to occupy less space on large monitors.
- `icons-only`: tuned build only. `true` draws each window as its app icon
  instead of a live preview (a macOS-style app switcher), with only the selected app's name under it (see
  [`label`](#label)). Off by default; applies live.
- `style`: tuned build only. How the windows are drawn: `"previews"` (live
  previews in rows, the default), `"icons"` (as `icons-only true`), or
  `"flip"`: every window live in a 3D stack, as Windows Vista's Flip 3D. Next
  to `icons-only` in the same block, `style` wins. Applies live.

  In the stack the selected window is at the front, each one behind it a
  step further back, up and to the left, all turned the same way. When the
  switcher appears, the windows on screen fly from where they are into the
  stack (the others fade in a step behind it); moving the selection scrolls the
  stack, the window leaving the front fading out towards you and coming back in
  at the far end; closing it flies every window back to where it is then, the
  chosen one following the view to it, before the desktop draws them again.
  The windows stay live throughout: their surfaces are drawn in perspective
  directly, never copied offscreen, so they keep getting frame callbacks. Up and
  Down step through the stack as Left and Right do; hovering selects nothing (the
  stack would move away under the pointer), a click picks a window. The selected
  window's icon and name sit on a pill under the stack; the scope panel at the
  top right, inside the area panels (a top bar) leave free. The grid's settings
  (sizes, gaps, the container and highlight) do not apply. The opening uses
  `animations.overview-open-close`, scrolling `window-movement`, closing
  `recent-windows-close`.

  Its shape is [`flip`](#flip).
- `icon-size`: tuned build only. Logical size of the icons in `icons-only`
  mode, 16 to 512 (default 96). When the rows don't fit, the icons, their
  highlight padding and the gaps shrink together; `max-height` and `max-scale`
  do not apply to them.
- `wrap`: tuned build only. `true` (the default, in either mode) keeps every
  preview or icon at full size and starts a new row when one is full; only
  when the rows outgrow the screen does everything shrink to fit. `false`
  keeps one row that shrinks to fit (icons shrink their padding and gaps too;
  previews keep theirs).
- `gap`: tuned build only. Space between two previews or icons, 0 to 256
  (default 16). A single row of icons shrinks it with the icons.

```kdl
recent-windows {
    previews {
        icons-only true
        icon-size 112
        // wrap true // a grid of icons instead of one row
    }
}
```

The tuned build lays the previews out in a **centered, wrapping grid** instead of
a single scrolling row. Every window is always on screen, and the thumbnails
adapt their size to the number of open windows: fewer windows produce bigger
previews, more windows shrink to fit. Each row (including a shorter last row) is
centered on the output, and the whole grid is centered vertically for the
current monitor, so there is neither wasted space nor off-screen windows.

`max-scale` and `max-height` are treated as upper bounds. The grid fit normally
determines the actual size; these keys only cap how large a preview may grow
when there are very few windows. To let a handful of windows fill the screen,
raise them; to keep previews modest, lower them.

```kdl
recent-windows {
    // Allow a few windows to grow large.
    previews {
        max-height 720
        max-scale 0.6
    }
}
```

```kdl
recent-windows {
    // Keep the previews smaller even with only one or two windows.
    previews {
        max-height 320
        max-scale 0.3
    }
}
```

### `scope-panel`

Tuned build only. Styles the scope indicator (`All`, `Workspace`, `Monitor`)
shown just above the switcher, drawn as a macOS-style segmented control: a
recessed track of equal segments, the current scope on a raised pill, a
hairline between two segments that are not current. Press <kbd>1</kbd>,
<kbd>2</kbd> or <kbd>3</kbd> while the switcher is open to change scope, or
click (or tap) a scope; hovering a scope brightens its label. Clicking the
panel keeps the switcher open.

- `off`: hide the panel (the scope keys still work).
- `background-color`, `background-opacity`: the track (default opacity 0.97).
- `text-color`: labels of the scopes that are not current.
- `active-color`: the current scope's pill. Its faint edge is white on a dark
  `background-color`, black on a light one.
- `active-text-color`: the current and the hovered scope's label.
- `border-color`, `border-width`: the track's outline in logical pixels (0
  disables it); `border-color` also draws the hairlines between segments.
- `corner-radius`: the track's corner radius; the pill's is 1.5 smaller.
- `font`: a Pango font description, e.g. `"Inter Medium 12px"`.

All settings hot-reload.

```kdl
recent-windows {
    scope-panel {
        background-color "#1f2a26"
        background-opacity 1
        text-color "#C1C497"
        active-color "#2d3a33"
        active-text-color "#e0e2c0"
        border-color "#2f3931"
        border-width 1
        corner-radius 7
        font "Inter Medium 12px"
    }
}
```

### `flip`

Tuned build only. The shape of the 3D stack (`previews { style "flip"; }`), in
real 3D: every window faces the same way, and each one sits straight behind
the one in front of it, so the stack goes back opposite to where the windows
face (facing right and down, it recedes left and up; facing you, straight
back). Lengths are percentages of the output's width or height. All
hot-reload.

- `tilt-x`: degrees the windows turn to face right (positive, their left
  edges nearer) or left (negative), -80 to 80 (default 23).
- `tilt-y`: then to face down (positive, their top edges nearer) or up,
  -80 to 80 (default 2).
- `spacing`: how far behind one another the windows are, along the stack, as
  a percentage of the output width, 0 to 200 (default 44).
- `offset-x`, `offset-y`: how far each window is shifted from the one in
  front of it, along the windows' own right and down directions as they lie
  on the screen (so the shift turns with the tilt, but never changes depth:
  the stack always goes away from you), as percentages of the output width,
  -100 to 100 (defaults 0). Facing you with `offset-x -8`, each window peeks out to
  the left of the one in front.
- `front-x`, `front-y`: the front window's centre, 0 to 100 (defaults 69
  and 60).
- `camera-x`, `camera-y`: where the stack is looked at from, relative to the
  front window's centre, as percentages of the output's width and height,
  -100 to 100 (defaults -15 and -32, Vista's: from the left and above, so
  the stack rises towards the top while the windows face nearly straight
  ahead; 0 and 0 look straight at it).
- `card-size`: the largest a window is drawn, as a percentage of the output's
  width and height, 5 to 100 (default 43); smaller windows keep their size.
- `visible-windows`: how many windows show at once, the front one included,
  1 to 50 (default 10, as Vista's Flip 3D); the rest come in at the back as
  the stack scrolls.

The older `tilt` (now `tilt-x`) still loads; `shift-x` and `shift-y` are
accepted and ignored, the tilt deciding where the stack goes.

```kdl
recent-windows {
    // Facing left and a little up: the stack goes back to the right and down.
    flip {
        tilt-x -24
        tilt-y -6
        front-x 40
    }
}
```

### `label`

Tuned build only. The label of each preview or icon. A window preview has a
header above it, as Windows' Alt-Tab: its app icon (24 px) at the left, the
label beside it, cut short to the preview's width. An icon's label goes under
it.

- `text`: what it says. `"title"` the window title, `"app-name"` the app's
  name (its desktop entry's `Name`, else the last part of its app id,
  `org.gnome.Nautilus` giving `Nautilus`), `"app-and-title"` both
  (`Firefox — GitHub`; just the name when the title only repeats it), or
  `"none"` for no labels. Default: `"title"` for previews, `"app-name"` for
  icons.
- `show`: `"all"` labels every preview or icon, inside its highlight;
  `"selected"` labels only the selected one (an icon's under its highlight,
  as wide as the container allows, as macOS's app switcher). Default: `"all"` for previews,
  `"selected"` for icons.
- `font`: Pango font description (default `"sans 14px"`).
- `gap`: space between the label and its icon, or between a preview's header
  and the preview, 0 to 256 (default 8).

The label colour is `container`'s `title-color`. All settings hot-reload.

```kdl
recent-windows {
    previews { icons-only true; }
    label {
        text "app-and-title"
        show "selected"
        font "Inter Medium 15px"
        gap 10
    }
}
```

### `container`

Tuned build only. The rounded panel behind the previews, over the blur of the
desktop.

- `background-color`, `background-opacity`: its tint (default black at 0.35).
- `title-color`: the labels under the previews (default white); pick one
  that reads on the tint, e.g. dark titles on a light tint.
- `padding`: space between the outermost highlights (or the selected label)
  and the panel's edge, 0 to 256 (default 16).
- `fullscreen`: `true` makes the panel cover the whole output, no corners or
  margins: the desktop all blurred and tinted behind the previews or icons
  (the scope panel at the top of the area panels leave free). Off by default.
- `overlay-opacity`: the tint's opacity, 0 to 1, its colour's own alpha aside
  (so a theme's colour and this layer independently). Unset, the colour's
  alpha times `background-opacity`.
- `blur-radius`: how strong the blur behind the panel is, in logical pixels,
  0 (none) to 1024. Unset, the global `blur` settings.
- `corner-radius`: the panel's corner radius. Unset, it is concentric with
  the highlight: the highlight's `corner-radius` plus its `border-width` plus
  `padding`, so the highlight's `corner-radius` rounds both.
- `margin-x`, `margin-y`: the least space between the panel and the left and
  right, or top and bottom, edges of the output (defaults 48 and 80). Rows
  wrap, and shrink once they run out of height, to stay inside them. A small
  `margin-y` lets the scope panel above overlap the container.

All settings hot-reload.

```kdl
recent-windows {
    container {
        background-color "#f5f5f7"
        background-opacity 0.6
        title-color "#1d1d1f"
    }
}
```

### `binds`

Configure binds that open and navigate the recent windows switcher.

The defaults are <kbd>Alt</kbd><kbd>Tab</kbd> / <kbd>Mod</kbd><kbd>Tab</kbd> to switch across all windows, and <kbd>Alt</kbd><kbd>\`</kbd> / <kbd>Mod</kbd><kbd>\`</kbd> to switch between windows of the current application.
Adding <kbd>Shift</kbd> will switch windows backwards.

Adding the recent windows `binds {}` section to your config removes all default binds.
You can copy the ones you need from the summary at the top of this wiki page.

```kdl
recent-windows {
    // Even an empty binds {} section will remove all default binds.
    binds {
    }
}
```

The available actions are `next-window` and `previous-window`.
They can optionally have the following properties:

- `filter="app-id"`: filters the switcher to the windows of the currently selected application, as determined by the Wayland app ID.
- `scope="all"`, `scope="output"`, `scope="workspace"`: sets the pre-selected scope when this bind is used to open the recent windows switcher.

```kdl
recent-windows {
    // Pre-select the "Output" scope when switching windows.
    binds {
        Mod+Tab         { next-window     scope="output"; }
        Mod+Shift+Tab   { previous-window scope="output"; }
        Mod+grave       { next-window     scope="output" filter="app-id"; }
        Mod+Shift+grave { previous-window scope="output" filter="app-id"; }
    }
}
```

The recent windows binds have lower precedence than the [normal binds](./Configuration:-Key-Bindings.md), meaning that if you have <kbd>Alt</kbd><kbd>Tab</kbd> bound to something else in the normal binds, the `recent-windows` bind won't work.
In this case, you can remove the conflicting normal bind.

All binds in this section must have a modifier key like <kbd>Alt</kbd> or <kbd>Mod</kbd> because the recent windows switcher remains open only while you hold any modifier key.

#### Bindings inside the switcher

When the switcher is open, these keys are available by default:

- <kbd>Escape</kbd> cancels the switcher.
- <kbd>Enter</kbd>, <kbd>Space</kbd> close the switcher confirming the current window.
- <kbd>1</kbd>, <kbd>2</kbd>, <kbd>3</kbd> select All, Workspace or Monitor (tuned build).
- <kbd>S</kbd> cycles between scopes, as indicated by the panel at the top.
- <kbd>←</kbd>, <kbd>→</kbd>, <kbd>↑</kbd>, <kbd>↓</kbd>, <kbd>Home</kbd>, <kbd>End</kbd> move the selection directionally.

Tuned build only: change them in `opened-binds`. Each key is written alone, without a
modifier (it is pressed while the modifier that opened the switcher is held); quote keys
that are numbers. A key listed here replaces its default, `none` removes one, the other
defaults stay. Actions: `confirm`, `cancel`, `next-window`, `previous-window`, `row-up`,
`row-down`, `first`, `last`, `set-scope "all"|"workspace"|"output"`, `cycle-scope`,
`close-window` (the selected window), and `spawn-for-window "cmd" "args"...`, which runs a
command with the selected window's id appended as its last argument, keeping the switcher
open. They hot-reload and take precedence over the regular binds listed below.

```kdl
recent-windows {
    opened-binds {
        W { close-window; }
        Q { spawn-for-window "my-quit-app" "--id"; }
        "4" { set-scope "output"; }
        S { none; }
    }
}
```

Additionally, certain regular binds will automatically work in the switcher:

- focus column left/right and their variants: will move the selection left/right inside the switcher.
- focus column first/last: will move the selection to the first or last window.
- close window: will close the window currently focused in the switcher.
- screenshot: will open the screenshot UI.

The way this works is by finding all regular binds corresponding to these actions and taking just the trigger key without modifiers.
For example, if you have <kbd>Mod</kbd><kbd>Shift</kbd><kbd>C</kbd> bound to `close-window`, in the window switcher pressing <kbd>C</kbd> on its own will close the window.

This way we don't need to hardcode things like HJKL directional movements.
If you have, say, Colemak-DH MNEI binds instead, they will work for you in the window switcher (as long as they don't conflict with the hardcoded ones).
