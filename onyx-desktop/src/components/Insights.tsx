// Vault Insights: all-local analytics. Nothing leaves the machine — the
// privacy stance is the feature.

import { For, Show, createResource } from "solid-js";

import { api } from "../api";
import { t } from "../i18n";

export default function Insights(props: {
  onOpen: (path: string) => void;
  onClose: () => void;
}) {
  const [stats] = createResource(() => api.vaultStats());

  return (
    <div class="overlay" onClick={() => props.onClose()}>
      <div class="palette insights" onClick={(event) => event.stopPropagation()}>
        <div class="settings-header">{t("insights.title")}</div>
        <Show when={stats()} fallback={<div class="palette-empty">…</div>}>
          {(data) => (
            <div class="insights-body">
              <div class="insights-tiles">
                <div class="insights-tile">
                  <b>{data().noteCount}</b>
                  <span>{t("insights.notes")}</span>
                </div>
                <div class="insights-tile">
                  <b>{data().totalWords.toLocaleString()}</b>
                  <span>{t("insights.words")}</span>
                </div>
                <div class="insights-tile">
                  <b>{data().linkCount}</b>
                  <span>{t("insights.links")}</span>
                </div>
                <div class="insights-tile">
                  <b>{data().attachmentCount}</b>
                  <span>{t("insights.attachments")}</span>
                </div>
                <div class="insights-tile">
                  <b>{data().orphanCount}</b>
                  <span>{t("insights.orphans")}</span>
                </div>
                <div class="insights-tile">
                  <b>{data().unresolvedCount}</b>
                  <span>{t("insights.unresolved")}</span>
                </div>
              </div>

              <div class="insights-columns">
                <section>
                  <h3>{t("insights.mostLinked")}</h3>
                  <For each={data().mostLinked}>
                    {(entry) => (
                      <button class="file-item" onClick={() => props.onOpen(entry.path)}>
                        {entry.path} · {entry.count}
                      </button>
                    )}
                  </For>
                </section>
                <section>
                  <h3>{t("insights.longest")}</h3>
                  <For each={data().longestNotes}>
                    {(entry) => (
                      <button class="file-item" onClick={() => props.onOpen(entry.path)}>
                        {entry.path} · {entry.count.toLocaleString()}
                      </button>
                    )}
                  </For>
                </section>
                <section>
                  <h3>{t("insights.topTags")}</h3>
                  <For each={data().topTags}>
                    {(entry) => (
                      <div class="file-item">
                        #{entry.tag} · {entry.count}
                      </div>
                    )}
                  </For>
                </section>
              </div>
              <div class="insights-footer">{t("insights.privacy")}</div>
            </div>
          )}
        </Show>
      </div>
    </div>
  );
}
