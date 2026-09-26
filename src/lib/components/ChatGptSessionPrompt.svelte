<script lang="ts">
  import { Check, ChevronDown, Copy, History } from "@lucide/svelte";
  import { onDestroy } from "svelte";
  import { showToast } from "$lib/stores/toast";

  const sessionPrompt = `请初始化或恢复当前项目会话。如果我的首次请求明确把当前 Workspace 内某个目录称为项目、repo、代码库、工程，或明显把该目录作为本轮主要开发对象，请先调用 set_active_project 将它绑定为当前对话的当前项目，再调用 history_session_bootstrap，并把我的首次请求逐字传入 initial_user_input。不要要求我手工选择，也不要要求该目录必须存在 .git、package.json、Cargo.toml 等项目标记。后续相对路径、History 和 Harness 都继续使用当前项目；仅引用其他文件或目录时不要误切项目。若我只给项目名而没有路径，可调用 discover_projects，只有唯一匹配时才自动切换。
如果没有历史记录，则创建首个 history-session；如果已有历史记录，先阅读返回的有界 state。请保留 bootstrap 返回的 session_key、current_path 和 project_id，并在 checkpoint 时原样传回；如果同一对话切换到另一个项目，请先为新项目重新 bootstrap/resume History，不要沿用旧项目的 checkpoint 目标。
需要早期精确细节时，先调用 history_session_search，再用 history_session_read 分页读取相关原始 Markdown，并根据 next_cursor 继续直到完成；不要要求 bootstrap 返回全部历史。
本会话每轮任务完成后调用 history_session_checkpoint，并原样传入 bootstrap 返回的 session_key 和 current_path，以及我本轮请求的逐字 raw_user_input。
只有 checkpoint 返回 ok=true 且会话目标一致后才能确认进度已保存；服务端不能自动读取未通过工具参数传入的对话内容。`;

  let copying = $state(false);
  let copied = $state(false);
  let expanded = $state(false);
  let errorMessage = $state("");
  let resetTimer: ReturnType<typeof setTimeout> | undefined;

  async function copyPrompt() {
    if (copying) return;
    copying = true;
    copied = false;
    errorMessage = "";
    if (resetTimer) clearTimeout(resetTimer);
    try {
      await navigator.clipboard.writeText(sessionPrompt);
      copied = true;
      showToast("新会话启动提示词已复制，可以直接粘贴到 ChatGPT。", {
        title: "复制成功",
        kind: "success",
        duration: 2500,
      });
      resetTimer = setTimeout(() => {
        copied = false;
      }, 2000);
    } catch (error) {
      errorMessage = "复制失败，请选中提示词后手动复制。";
      showToast(String(error), {
        title: "无法复制提示词",
        kind: "error",
        duration: 6000,
      });
    } finally {
      copying = false;
    }
  }

  onDestroy(() => {
    if (resetTimer) clearTimeout(resetTimer);
  });
</script>

<section
  class="rounded-[12px] border border-[var(--color-border)] bg-[var(--card-bg)] px-3 py-2.5 sm:px-4"
  aria-labelledby="chatgpt-session-prompt-title"
>
  <div class="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
    <div class="flex min-w-0 items-center gap-3">
      <span
        class="flex size-9 shrink-0 items-center justify-center rounded-[10px] bg-[var(--primary-soft)] text-[var(--primary)]"
        aria-hidden="true"
      >
        <History size={16} />
      </span>
      <div class="min-w-0">
        <h3 id="chatgpt-session-prompt-title" class="text-sm font-semibold text-[var(--color-text)]">
          ChatGPT 新会话启动提示词
        </h3>
        <p class="mt-0.5 text-xs leading-5 text-[var(--color-text-muted)]">
          首次使用会初始化历史；后续新会话会自动恢复已有进度。
        </p>
      </div>
    </div>

    <div class="flex shrink-0 flex-wrap items-center gap-2 sm:flex-nowrap">
      <button
        type="button"
        class="tx-btn-primary min-h-11 shrink-0 px-3 py-2 text-xs active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-50"
        disabled={copying}
        aria-label="复制 ChatGPT 新会话启动提示词"
        onclick={() => void copyPrompt()}
      >
        {#if copied}
          <Check size={14} aria-hidden="true" />
          <span>已复制</span>
        {:else}
          <Copy size={14} aria-hidden="true" />
          <span>{copying ? "复制中…" : "复制完整提示词"}</span>
        {/if}
      </button>

      <button
        type="button"
        class="tx-btn-ghost min-h-11 shrink-0 gap-1.5 px-3 py-2 text-xs active:scale-[0.98]"
        aria-expanded={expanded}
        aria-controls="chatgpt-session-prompt-content"
        onclick={() => (expanded = !expanded)}
      >
        <span>{expanded ? "收起提示词" : "查看完整提示词"}</span>
        <ChevronDown
          size={14}
          class={`transition-transform duration-200 motion-reduce:transition-none ${expanded ? "rotate-180" : ""}`}
          aria-hidden="true"
        />
      </button>
    </div>
  </div>

  {#if expanded}
    <div id="chatgpt-session-prompt-content" class="mt-3 border-t border-[var(--color-border)] pt-3">
      <pre
        class="tx-mono whitespace-pre-wrap break-words rounded-[10px] bg-[var(--surface-hover)] p-3 leading-5 text-[var(--color-text-secondary)]"
      >{sessionPrompt}</pre>
      <p class="mt-2 text-[11px] leading-5 text-[var(--color-text-muted)]">
        复制后粘贴到使用当前工作区 MCP 连接器的 ChatGPT 新会话。
      </p>
    </div>
  {/if}

  {#if errorMessage}
    <p class="mt-2 text-xs text-[var(--danger)]" role="alert">{errorMessage}</p>
  {/if}
  <span class="sr-only" aria-live="polite">{copied ? "提示词已复制" : ""}</span>
</section>
