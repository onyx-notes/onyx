// The vault graph: WebGL rendering via sigma, ForceAtlas2 layout, click a
// node to open the note. Full-screen overlay (Ctrl+G).

import Graph from "graphology";
import forceAtlas2 from "graphology-layout-forceatlas2";
import Sigma from "sigma";
import { Show, createSignal, onCleanup, onMount } from "solid-js";

import { api } from "../api";
import { t } from "../i18n";

export default function GraphView(props: {
  onOpen: (path: string) => void;
  onClose: () => void;
}) {
  let host!: HTMLDivElement;
  let renderer: Sigma | undefined;
  const [stats, setStats] = createSignal("");
  const [empty, setEmpty] = createSignal(false);

  onMount(async () => {
    const payload = await api.graphPayload();
    if (payload.nodes.length === 0) {
      setEmpty(true);
      return;
    }

    const graph = new Graph({ type: "undirected", multi: false });
    payload.nodes.forEach((node, index) => {
      // Deterministic disc seeding: stable layouts across opens.
      const angle = (index / payload.nodes.length) * Math.PI * 2;
      const radius = 1 + Math.sqrt(index);
      graph.addNode(String(index), {
        label: node.title,
        x: Math.cos(angle) * radius,
        y: Math.sin(angle) * radius,
        size: Math.min(3 + Math.sqrt(node.degree) * 2, 14),
        color: node.degree > 0 ? "#8b7ff5" : "#71717e",
        path: node.path,
      });
    });
    for (const [source, target] of payload.edges) {
      const a = String(source);
      const b = String(target);
      if (!graph.hasEdge(a, b) && a !== b) {
        graph.addEdge(a, b, { color: "#30303c", size: 1 });
      }
    }

    forceAtlas2.assign(graph, {
      iterations: Math.min(300, 50 + payload.nodes.length),
      settings: forceAtlas2.inferSettings(graph),
    });

    renderer = new Sigma(graph, host, {
      renderLabels: payload.nodes.length <= 400,
      labelColor: { color: "#a6a6b4" },
      labelSize: 11,
    });
    renderer.on("clickNode", ({ node }) => {
      const path = graph.getNodeAttribute(node, "path") as string;
      props.onClose();
      props.onOpen(path);
    });

    setStats(
      t("graph.stats", {
        nodes: payload.nodes.length,
        edges: payload.edges.length,
      }),
    );
  });

  onCleanup(() => renderer?.kill());

  return (
    <div class="overlay" onClick={() => props.onClose()}>
      <div class="graph-panel" onClick={(event) => event.stopPropagation()}>
        <div class="settings-header">
          <span>{t("graph.title")}</span>
          <span class="settings-caps">{stats()}</span>
        </div>
        <Show when={!empty()} fallback={<div class="palette-empty">{t("graph.empty")}</div>}>
          <div class="graph-host" ref={host} />
        </Show>
      </div>
    </div>
  );
}
