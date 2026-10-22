"use client";

/// 音声のプレーヤー（#784）。ブラウザ標準の `<audio controls>` はブラウザごとに見た目が違い、
/// 画面から浮くので、操作は自前で描いて `<audio>` は裏で鳴らすだけにする。
///
/// 再生・一時停止（Space）、15 秒戻る／進む（←／→）、シーク（ドラッグ・クリック・キーボード）、
/// 再生速度、ミュート。色は季節のアクセント（ローディングと同じ）。
import * as React from "react";
import { Pause, Play, RotateCcw, RotateCw, Volume2, VolumeX } from "lucide-react";

import { currentSeasonIndex, seasonVar } from "@/lib/season";
import { cn } from "@/lib/utils";

const SKIP_SECONDS = 15;
const RATES = [1, 1.25, 1.5, 2, 0.75];

function formatTime(sec: number): string {
  if (!Number.isFinite(sec) || sec < 0) return "0:00";
  const s = Math.floor(sec);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

export function AudioPlayer({
  src,
  title,
  meta,
  mediaRef,
  onError,
  onLoadedMetadata,
  onTimeUpdate,
}: {
  src: string;
  title: string;
  meta: string;
  mediaRef: (el: HTMLAudioElement | null) => void;
  onError: () => void;
  onLoadedMetadata: () => void;
  onTimeUpdate: () => void;
}) {
  const audioRef = React.useRef<HTMLAudioElement | null>(null);
  const [playing, setPlaying] = React.useState(false);
  const [time, setTime] = React.useState(0);
  const [duration, setDuration] = React.useState(0);
  const [buffered, setBuffered] = React.useState(0);
  const [rate, setRate] = React.useState(1);
  const [muted, setMuted] = React.useState(false);
  const [scrub, setScrub] = React.useState<number | null>(null);
  const accent = React.useMemo(() => seasonVar(currentSeasonIndex()), []);

  const toggle = React.useCallback(() => {
    const a = audioRef.current;
    if (!a) return;
    if (a.paused) void a.play().catch(() => undefined);
    else a.pause();
  }, []);
  const seekTo = React.useCallback((t: number) => {
    const a = audioRef.current;
    if (!a || !Number.isFinite(a.duration)) return;
    a.currentTime = Math.min(a.duration, Math.max(0, t));
    setTime(a.currentTime);
  }, []);
  const skip = React.useCallback((d: number) => seekTo((audioRef.current?.currentTime ?? 0) + d), [seekTo]);

  // キーボード（入力欄にいないとき）。
  React.useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT|BUTTON)$/.test(t.tagName))) return;
      // メニュー・ダイアログを操作中のキーは横取りしない（⋯ メニューを Space で選ぶなど）。
      if (t?.closest('[role="menu"],[role="dialog"],[role="listbox"]')) return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key === " ") {
        e.preventDefault();
        toggle();
      } else if (e.key === "ArrowLeft") {
        e.preventDefault();
        skip(-SKIP_SECONDS);
      } else if (e.key === "ArrowRight") {
        e.preventDefault();
        skip(SKIP_SECONDS);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [toggle, skip]);

  // シークバー（ポインタでつまんで動かす。離したところで確定）。
  const barRef = React.useRef<HTMLDivElement>(null);
  const ratioAt = (clientX: number) => {
    const r = barRef.current?.getBoundingClientRect();
    if (!r || r.width === 0) return 0;
    return Math.min(1, Math.max(0, (clientX - r.left) / r.width));
  };
  const shown = scrub ?? (duration > 0 ? time / duration : 0);

  return (
    <div className="flex w-full max-w-md flex-col gap-6 rounded-3xl border border-border/60 bg-card p-5 shadow-[0_1px_2px_rgb(0_0_0/0.04),0_16px_40px_-12px_rgb(0_0_0/0.16)] sm:p-6">
      {/* ジャケット代わりの面。再生中は波形のバーが動く。 */}
      <div
        className="relative flex aspect-[16/9] items-end justify-between gap-[4px] overflow-hidden rounded-2xl px-6 pb-8 sm:px-8"
        style={{
          background: `radial-gradient(120% 120% at 0% 0%, color-mix(in oklab, ${accent} 38%, transparent), transparent 60%), radial-gradient(120% 120% at 100% 100%, color-mix(in oklab, ${accent} 22%, transparent), transparent 55%), var(--muted)`,
        }}
        aria-hidden
      >
        {Array.from({ length: 24 }, (_, i) => (
          <span
            key={i}
            className={cn("audio-bar w-[5px] min-w-[3px] shrink rounded-full", playing && "audio-bar-playing")}
            style={{
              background: `color-mix(in oklab, ${accent} 75%, var(--foreground))`,
              height: `${18 + Math.round(42 * Math.abs(Math.sin(i * 1.7) * Math.cos(i * 0.6)))}%`,
              animationDelay: `${(i % 7) * -0.17}s`,
              opacity: (i + 0.5) / 24 <= shown ? 0.95 : 0.35,
            }}
          />
        ))}
      </div>

      <div className="flex min-w-0 flex-col gap-1">
        <h1 className="truncate text-base font-semibold leading-snug" title={title}>
          {title}
        </h1>
        <p className="text-xs text-muted-foreground">{meta}</p>
      </div>

      <div className="flex flex-col gap-2">
        <div
          ref={barRef}
          role="slider"
          tabIndex={0}
          aria-label="再生位置"
          aria-valuemin={0}
          aria-valuemax={Math.round(duration)}
          aria-valuenow={Math.round(time)}
          aria-valuetext={`${formatTime(time)} / ${formatTime(duration)}`}
          className="group relative flex h-5 cursor-pointer touch-none items-center rounded-full outline-none focus-visible:ring-2 focus-visible:ring-ring"
          onPointerDown={(e) => {
            e.currentTarget.setPointerCapture(e.pointerId);
            setScrub(ratioAt(e.clientX));
          }}
          onPointerMove={(e) => {
            if (scrub != null) setScrub(ratioAt(e.clientX));
          }}
          onPointerUp={(e) => {
            if (scrub == null) return;
            seekTo(ratioAt(e.clientX) * duration);
            setScrub(null);
          }}
          onPointerCancel={() => setScrub(null)}
          onKeyDown={(e) => {
            if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
              e.preventDefault();
              e.stopPropagation();
              skip(e.key === "ArrowLeft" ? -5 : 5);
            }
          }}
          data-testid="audio-seek"
        >
          <div className="relative h-1.5 w-full overflow-hidden rounded-full bg-foreground/10">
            <div className="absolute inset-y-0 left-0 rounded-full bg-foreground/10" style={{ width: `${buffered * 100}%` }} />
            <div className="absolute inset-y-0 left-0 rounded-full" style={{ width: `${shown * 100}%`, background: accent }} />
          </div>
          <div
            className={cn(
              "absolute top-1/2 size-3.5 -translate-x-1/2 -translate-y-1/2 rounded-full shadow ring-2 ring-card transition-transform",
              scrub != null ? "scale-125" : "scale-0 group-hover:scale-100 group-focus-visible:scale-100 pointer-coarse:scale-100",
            )}
            style={{ left: `${shown * 100}%`, background: accent }}
          />
        </div>
        <div className="flex justify-between text-[11px] font-medium tabular-nums text-muted-foreground">
          <span data-testid="audio-time">{formatTime(scrub != null ? scrub * duration : time)}</span>
          <span>{formatTime(duration)}</span>
        </div>
      </div>

      <div className="grid grid-cols-[1fr_auto_1fr] items-center">
        <button
          type="button"
          onClick={() => {
            const next = RATES[(RATES.indexOf(rate) + 1) % RATES.length];
            setRate(next);
            if (audioRef.current) audioRef.current.playbackRate = next;
          }}
          className="justify-self-start rounded-full px-2.5 py-1 text-xs font-semibold tabular-nums text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:py-2"
          aria-label={`再生速度 ${rate} 倍`}
          title="再生速度"
          data-testid="audio-rate"
        >
          {rate}×
        </button>
        <div className="flex items-center gap-3">
          <RoundButton label={`${SKIP_SECONDS} 秒戻る（←）`} onClick={() => skip(-SKIP_SECONDS)}>
            <RotateCcw aria-hidden />
          </RoundButton>
          <button
            type="button"
            onClick={toggle}
            aria-label={playing ? "一時停止（Space）" : "再生（Space）"}
            title={playing ? "一時停止（Space）" : "再生（Space）"}
            style={{ background: `color-mix(in oklab, ${accent} 88%, var(--foreground))` }}
            className="flex size-14 items-center justify-center rounded-full text-white shadow-md outline-none transition-transform hover:scale-[1.04] focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card active:scale-[0.96] [&_svg]:size-6"
            data-testid="audio-play"
          >
            {playing ? <Pause aria-hidden className="fill-current" /> : <Play aria-hidden className="ml-0.5 fill-current" />}
          </button>
          <RoundButton label={`${SKIP_SECONDS} 秒進む（→）`} onClick={() => skip(SKIP_SECONDS)}>
            <RotateCw aria-hidden />
          </RoundButton>
        </div>
        <button
          type="button"
          onClick={() => {
            const a = audioRef.current;
            if (!a) return;
            a.muted = !a.muted;
            setMuted(a.muted);
          }}
          className="flex size-9 items-center justify-center justify-self-end rounded-full text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:size-11 [&_svg]:size-[18px]"
          aria-label={muted ? "ミュートを解除" : "ミュート"}
          title={muted ? "ミュートを解除" : "ミュート"}
        >
          {muted ? <VolumeX aria-hidden /> : <Volume2 aria-hidden />}
        </button>
      </div>

      <audio
        ref={(el) => {
          audioRef.current = el;
          mediaRef(el);
        }}
        src={src}
        preload="metadata"
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onEnded={() => setPlaying(false)}
        onTimeUpdate={(e) => {
          setTime(e.currentTarget.currentTime);
          onTimeUpdate();
        }}
        onDurationChange={(e) => setDuration(Number.isFinite(e.currentTarget.duration) ? e.currentTarget.duration : 0)}
        onProgress={(e) => {
          const a = e.currentTarget;
          if (a.buffered.length > 0 && a.duration > 0) setBuffered(a.buffered.end(a.buffered.length - 1) / a.duration);
        }}
        onError={onError}
        onLoadedMetadata={(e) => {
          e.currentTarget.playbackRate = rate;
          onLoadedMetadata();
        }}
        className="hidden"
        data-testid="audio-preview-media"
      />
    </div>
  );
}

function RoundButton({ label, onClick, children }: { label: string; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      className="flex size-10 items-center justify-center rounded-full text-foreground/75 outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring active:scale-[0.94] [&_svg]:size-[18px]"
    >
      {children}
    </button>
  );
}
