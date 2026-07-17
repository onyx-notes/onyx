// Reading view: the Rust renderer's HTML, enhanced with math, diagrams,
// vault images, and clickable wikilinks. Ctrl+R toggles it per tab.

import { createEffect, on, onCleanup } from "solid-js";

import { api } from "../api";
import { enhanceRendered } from "../editor/enhance";

export default function ReadingView(props: {
  path: string;
  /** Re-render trigger (external edits bump this). */
  reloadSignal: number;
  onFollowLink: (target: string) => void;
}) {
  let host!: HTMLDivElement;
  let generation = 0;

  const render = async (path: string) => {
    const current = ++generation;
    try {
      const html = await api.renderNote(path);
      if (current !== generation) return; // superseded
      host.innerHTML = html;
      await enhanceRendered(host, { followLink: props.onFollowLink });
    } catch (error) {
      host.textContent = String(error);
    }
  };

  createEffect(
    on(
      () => [props.path, props.reloadSignal] as const,
      ([path]) => void render(path),
    ),
  );

  onCleanup(() => {
    generation += 1;
  });

  return <div class="reading-view" ref={host} />;
}
