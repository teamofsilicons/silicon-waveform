# UIArc source adaptations

The local `src/uiarc.css` ports the free UIArc Button, Card, Input and Textarea
CSS into Waveform's existing Solid components. Existing native inputs, dialogs,
request state and focus restoration remain owned by the application. Card quick-look dialogs and React/Motion label
morphing are not included. No Pro components or runtime React dependency are used.

Source: [UIArc](https://uiarc.dev/), official
[free registry](https://github.com/kuratlielia/arc-library/tree/792791245398f1009a0054544a02fb4f3455df07/public/r),
pinned commit `792791245398f1009a0054544a02fb4f3455df07`.

Adapted files:
- `registry/foundation.css`: control sizes, spacing, radii, transition timing.
- `registry/components/button/button.module.css`: variants, disabled/busy states, focus and pointer feedback.
- `registry/components/card/card.module.css`: flat bordered surface, content spacing and title hierarchy.
- `registry/components/input/input.module.css` and `textarea/textarea.module.css`: field shape, invalid/focus/disabled states.

Application changes map the neutral tokens to the existing blue brand and light
surfaces, retain visible focus rings and native semantics, and respect reduced
motion. Form layout and copy are application-specific. The MIT copyright and
license are distributed in
[`public/licenses/UIArc.txt`](public/licenses/UIArc.txt).

Registry source SHA-256:

- `arc-foundation.json`: `707e3576a82b429966996265386efe6c7dd221fd9628cabd8adffef0ce6c2ece`
- `button.json`: `34b3fab633e91f5772ba9796d5c87088a15f763d63afd2b7e0bb26bc775180b5`
- `card.json`: `761c584e7b95d99d15ba30985bf340e6c911fe497c8fed2925c77caf051d65ce`
- `input.json`: `5073c6ab18f876b2b307a3434fcffd479ade5ab31cfa1b95e456c75ea9a4f9df`
- `textarea.json`: `228d56f8bb945e4049fda8236a7fa0b8adf098cce799453610e96634f61defa1`
- `LICENSE`: `1b73a3fbab233cb995e6e9b6a63d302f91975c74c1629919c8e9820902820dde`
