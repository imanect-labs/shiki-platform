import type { Metadata } from "next";
import type { LucideIcon } from "lucide-react";
import {
  ArrowUpDown,
  Bell,
  CalendarDays,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  ClipboardList,
  Eye,
  FileText,
  Globe,
  Home,
  Inbox,
  Layers,
  LayoutGrid,
  LayoutTemplate,
  List,
  Lock,
  MessagesSquare,
  MoreHorizontal,
  Music2,
  PanelLeft,
  Play,
  Plus,
  PlusCircle,
  RotateCw,
  Search,
  Share,
  ShoppingBag,
  SlidersHorizontal,
  Sparkles,
  Table2,
  Users,
} from "lucide-react";

export const metadata: Metadata = { title: "Reference" };

/* 参照デザイン(boards.com)の忠実な写経。アクセントは紫を排しブルー基調。
   これをフロントのデザイン品質の基準線とする（後で shiki ドメインへ写像）。 */

// グラデーション（紫/マゼンタは不使用）
const G = {
  brand: "linear-gradient(135deg,#36D1C4 0%,#2F8BFF 100%)",
  blue: "linear-gradient(135deg,#5BA6FF 0%,#2F6BFF 100%)",
  cyan: "linear-gradient(135deg,#41D6F2 0%,#2AA6E6 100%)",
  teal: "linear-gradient(135deg,#4BE7B0 0%,#1FC58A 100%)",
  orange: "linear-gradient(135deg,#FFB057 0%,#FF7A3D 100%)",
  rose: "linear-gradient(135deg,#FF8FB1 0%,#FF5C7A 100%)",
  amber: "linear-gradient(135deg,#FFD15C 0%,#FFA92E 100%)",
  red: "linear-gradient(135deg,#FF7B7B 0%,#EE4B4B 100%)",
  dark: "linear-gradient(135deg,#3A3A3F 0%,#16161A 100%)",
  green: "linear-gradient(135deg,#7BE08A 0%,#33B25A 100%)",
};

const ACCENT = "#2F6BFF";

function Tile({
  grad,
  icon: Icon,
  size = 22,
  radius = 7,
}: {
  grad: string;
  icon?: LucideIcon;
  size?: number;
  radius?: number;
}) {
  return (
    <span
      className="flex shrink-0 items-center justify-center shadow-[0_1px_2px_rgba(0,0,0,0.12)] ring-1 ring-black/5"
      style={{ background: grad, width: size, height: size, borderRadius: radius }}
    >
      {Icon ? <Icon className="text-white/95" style={{ width: size * 0.5, height: size * 0.5 }} /> : null}
    </span>
  );
}

/* ───────── サイドバー ───────── */

function NavRow({
  icon: Icon,
  label,
  active,
  trailing,
}: {
  icon: LucideIcon;
  label: string;
  active?: boolean;
  trailing?: React.ReactNode;
}) {
  return (
    <button
      type="button"
      className={[
        "group flex h-[34px] w-full items-center gap-2.5 rounded-[9px] px-2.5 text-[13.5px] transition-colors",
        active
          ? "border border-[#ececec] bg-white font-medium text-[#23232a] shadow-[0_1px_2px_rgba(20,20,40,0.06)]"
          : "text-[#5b5b63] hover:bg-[#ededee]",
      ].join(" ")}
    >
      <Icon
        className={["size-[18px] shrink-0", active ? "text-[#23232a]" : "text-[#7e7e86]"].join(" ")}
        strokeWidth={2}
      />
      <span className="flex-1 truncate text-left">{label}</span>
      {trailing}
    </button>
  );
}

function CountBadge({ value }: { value: string }) {
  return <span className="text-[12px] tabular-nums text-[#a0a0a7]">{value}</span>;
}

function SectionHeader({ label, action }: { label: string; action: React.ReactNode }) {
  return (
    <div className="flex h-7 items-center justify-between px-2.5 pt-1">
      <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-[#9a9aa1]">
        {label}
      </span>
      <span className="text-[#c2c2c8]">{action}</span>
    </div>
  );
}

function ProjectRow({
  grad,
  icon,
  label,
  open,
}: {
  grad: string;
  icon: LucideIcon;
  label: string;
  open?: boolean;
}) {
  const Chevron = open ? ChevronUp : ChevronDown;
  return (
    <button
      type="button"
      className="group flex h-[34px] w-full items-center gap-2.5 rounded-[9px] px-2 text-[13.5px] text-[#46464d] transition-colors hover:bg-[#ededee]"
    >
      <Tile grad={grad} icon={icon} size={22} />
      <span className="flex-1 truncate text-left">{label}</span>
      <Chevron className="size-3.5 text-[#b6b6bc]" />
    </button>
  );
}

function MonthRow({ label, count }: { label: string; count: string }) {
  return (
    <button
      type="button"
      className="flex h-[30px] w-full items-center justify-between rounded-[9px] pl-[42px] pr-2.5 text-[13px] text-[#6b6b72] transition-colors hover:bg-[#ededee]"
    >
      <span className="truncate">{label}</span>
      <CountBadge value={count} />
    </button>
  );
}

function Sidebar() {
  return (
    <aside className="flex w-[256px] shrink-0 flex-col border-r border-[#ececec] bg-[#f7f7f8]">
      {/* ワークスペース切替 */}
      <div className="flex h-[52px] items-center gap-2 px-3">
        <Tile grad={G.brand} size={26} radius={8} />
        <span className="text-[15px] font-semibold tracking-[-0.01em] text-[#23232a]">Starline™</span>
        <ChevronDown className="size-4 text-[#9a9aa1]" />
        <button
          type="button"
          aria-label="サイドバーを折りたたむ"
          className="ml-auto flex size-7 items-center justify-center rounded-md text-[#9a9aa1] hover:bg-[#ededee]"
        >
          <PanelLeft className="size-[18px]" />
        </button>
      </div>

      {/* 検索 */}
      <div className="px-3 pb-1">
        <div className="relative">
          <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-[#a0a0a7]" />
          <input
            placeholder="Search"
            aria-label="Search"
            className="h-9 w-full rounded-[10px] border border-[#ececec] bg-white pl-9 pr-9 text-[13px] text-[#23232a] shadow-[0_1px_1px_rgba(20,20,40,0.03)] outline-none placeholder:text-[#a0a0a7] focus-visible:border-[#d6d6dc]"
          />
          <kbd className="absolute right-2.5 top-1/2 -translate-y-1/2 rounded-[5px] border border-[#e6e6ea] bg-[#fafafa] px-1.5 py-0.5 text-[11px] leading-none text-[#b4b4ba]">
            /
          </kbd>
        </div>
      </div>

      {/* スクロール領域 */}
      <div className="flex-1 overflow-y-auto px-3 pb-2">
        {/* 一次ナビ */}
        <nav className="flex flex-col gap-0.5 pt-1">
          <NavRow icon={Home} label="Home" />
          <NavRow icon={Bell} label="Updates" trailing={<CountBadge value="44" />} />
          <NavRow icon={Inbox} label="Inbox" trailing={<CountBadge value="20" />} />
          <NavRow
            icon={ClipboardList}
            label="My tasks"
            trailing={<Plus className="size-4 text-[#a0a0a7]" />}
          />
        </nav>

        {/* WORKSPACE */}
        <SectionHeader label="Workspace" action={<MoreHorizontal className="size-4" />} />
        <nav className="flex flex-col gap-0.5">
          <NavRow icon={FileText} label="Reports" />
          <NavRow icon={Globe} label="Companies" active />
          <NavRow icon={Layers} label="Projects" trailing={<ChevronDown className="size-3.5 text-[#b6b6bc]" />} />
          <NavRow
            icon={LayoutTemplate}
            label="Templates"
            trailing={<ChevronDown className="size-3.5 text-[#b6b6bc]" />}
          />
          <NavRow icon={Table2} label="Views" trailing={<ChevronDown className="size-3.5 text-[#b6b6bc]" />} />
          <NavRow icon={Users} label="Teams" trailing={<CountBadge value="48" />} />
        </nav>

        {/* PROJECTS */}
        <SectionHeader label="Projects" action={<Plus className="size-4" />} />
        <nav className="flex flex-col gap-0.5">
          <ProjectRow grad={G.blue} icon={CalendarDays} label="Tuesday™" open />
          <MonthRow label="January" count="12" />
          <MonthRow label="February" count="23" />
          <ProjectRow grad={G.dark} icon={Music2} label="Jammio™" />
          <ProjectRow grad={G.cyan} icon={Sparkles} label="Create™ AI" open />
          <MonthRow label="March" count="99" />
          <ProjectRow grad={G.teal} icon={MessagesSquare} label="Thoughts™" />
          <ProjectRow grad={G.orange} icon={ShoppingBag} label="Consumex™" open />
          <MonthRow label="January" count="12" />
          <MonthRow label="February" count="23" />
        </nav>
      </div>

      {/* アカウント */}
      <div className="border-t border-[#ececec] p-3">
        <button
          type="button"
          className="flex w-full items-center gap-2.5 rounded-[10px] p-1.5 transition-colors hover:bg-[#ededee]"
        >
          <Tile grad={G.rose} size={28} radius={999} />
          <span className="flex-1 text-left text-[13.5px] font-medium text-[#23232a]">Lee Cooper</span>
          <MoreHorizontal className="size-4 text-[#a0a0a7]" />
        </button>
      </div>
    </aside>
  );
}

/* ───────── ツールバー ───────── */

function ToolButton({
  icon: Icon,
  label,
}: {
  icon: LucideIcon;
  label?: string;
}) {
  return (
    <button
      type="button"
      className="flex h-8 items-center gap-1.5 rounded-[9px] border border-[#ececec] bg-white px-2.5 text-[13px] text-[#46464d] shadow-[0_1px_1px_rgba(20,20,40,0.03)] transition-colors hover:bg-[#fafafa]"
    >
      <Icon className="size-4 text-[#6b6b72]" />
      {label ? <span>{label}</span> : null}
    </button>
  );
}

function Toolbar() {
  return (
    <div className="flex h-[56px] items-center gap-2 border-b border-[#f0f0f1] px-5">
      <Globe className="size-[18px] text-[#23232a]" />
      <span className="text-[15px] font-semibold tracking-[-0.01em] text-[#23232a]">Companies</span>
      <MoreHorizontal className="size-4 text-[#b6b6bc]" />

      <div className="ml-auto flex items-center gap-2">
        {/* 表示切替セグメント */}
        <div className="flex items-center gap-0.5 rounded-[9px] border border-[#ececec] bg-white p-0.5 shadow-[0_1px_1px_rgba(20,20,40,0.03)]">
          <button
            type="button"
            aria-label="ボード表示"
            className="flex size-7 items-center justify-center rounded-[7px] bg-[#f1f1f3] text-[#23232a]"
          >
            <LayoutGrid className="size-4" />
          </button>
          <button
            type="button"
            aria-label="リスト表示"
            className="flex size-7 items-center justify-center rounded-[7px] text-[#9a9aa1] hover:text-[#46464d]"
          >
            <List className="size-4" />
          </button>
        </div>

        <ToolButton icon={Eye} label="Customize" />
        <ToolButton icon={ArrowUpDown} label="Sort" />
        <ToolButton icon={SlidersHorizontal} label="Filter" />
        <ToolButton icon={Search} />

        <button
          type="button"
          className="flex h-8 items-center gap-1.5 rounded-[9px] px-3 text-[13px] font-medium text-white shadow-[0_1px_2px_rgba(47,107,255,0.45)]"
          style={{ background: G.blue }}
        >
          <PlusCircle className="size-4" />
          New
        </button>
      </div>
    </div>
  );
}

/* ───────── テーブル ───────── */

const COLS = "grid-cols-[minmax(260px,1.5fr)_minmax(150px,1fr)_minmax(130px,0.9fr)_minmax(160px,1fr)_minmax(190px,1.1fr)]";

function HeaderCell({ label }: { label: string }) {
  return (
    <div className="flex items-center gap-1.5 text-[12.5px] font-medium text-[#9a9aa1]">
      <span>{label}</span>
      <span className="flex size-3.5 items-center justify-center rounded-full border border-[#dadadf] text-[9px] leading-none text-[#b4b4ba]">
        i
      </span>
    </div>
  );
}

function CheckBox({ checked }: { checked?: boolean }) {
  return (
    <span
      className={[
        "flex size-[18px] shrink-0 items-center justify-center rounded-[6px] border",
        checked ? "border-transparent text-white" : "border-[#d2d2d8] bg-white",
      ].join(" ")}
      style={checked ? { background: ACCENT } : undefined}
    >
      {checked ? <Check className="size-3" strokeWidth={3} /> : null}
    </span>
  );
}

type Row = {
  grad: string;
  icon: LucideIcon;
  name: string;
  domain: string;
  funding: string;
  created: string;
  updated: string;
};

const ROWS: Row[] = [
  { grad: G.rose, icon: ShoppingBag, name: "Solution Tech", domain: "solution.tech", funding: "$10M - $20M", created: "March 15th, 2024", updated: "9:30 AM, July 22nd, 2025" },
  { grad: G.blue, icon: Sparkles, name: "Pixel Tech", domain: "synergy.app", funding: "$23.5M", created: "July 22nd, 2025", updated: "4:15 PM, November 5th, 2023" },
  { grad: G.red, icon: Layers, name: "Innovate Solutions", domain: "apex.tech", funding: "$100M - $150M", created: "November 5th, 2023", updated: "10:02 AM, January 30th, 2027" },
  { grad: G.amber, icon: Globe, name: "Synergy Tech", domain: "cortex.com.au", funding: "$300M", created: "January 30th, 2027", updated: "2:50 PM, April 19th, 2026" },
  { grad: G.teal, icon: Table2, name: "Tech Company", domain: "techcompany.com", funding: "$45M", created: "April 19th, 2026", updated: "8:20 AM, September 18th, 2024" },
  { grad: G.cyan, icon: MessagesSquare, name: "Open Industries", domain: "openind.io", funding: "$12M", created: "September 18th, 2024", updated: "1:10 PM, March 3rd, 2025" },
  { grad: G.orange, icon: Music2, name: "Cortex Labs", domain: "cortexlabs.com", funding: "$78M", created: "March 3rd, 2025", updated: "11:45 AM, June 8th, 2026" },
];

function TableCell({ children }: { children: React.ReactNode }) {
  return <div className="self-center truncate text-[13.5px] text-[#5b5b63]">{children}</div>;
}

function CompaniesTable() {
  return (
    <div className="relative">
      {/* ヘッダ */}
      <div className={`grid ${COLS} items-center border-b border-[#f0f0f1] py-3 pl-[52px] pr-6`}>
        <HeaderCell label="Company (120)" />
        <HeaderCell label="Domains" />
        <HeaderCell label="Funding" />
        <HeaderCell label="Created on" />
        <HeaderCell label="Last updated" />
      </div>

      {/* 行 */}
      <div>
        {ROWS.map((r, i) => (
          <div
            key={r.name}
            className={`group grid ${COLS} items-center border-b border-[#f4f4f5] py-2.5 pl-4 pr-6 hover:bg-[#fafafa]`}
            style={{ opacity: Math.max(0.18, 1 - i * 0.14) }}
          >
            <div className="flex items-center gap-3">
              <CheckBox />
              <Tile grad={r.grad} icon={r.icon} size={22} />
              <span className="truncate text-[13.5px] font-medium text-[#23232a]">{r.name}</span>
              <span className="text-[13px] text-[#c6c6cc] opacity-0 transition-opacity group-hover:opacity-100">
                ⋯
              </span>
            </div>
            <TableCell>
              <span className="text-[#2F6BFF]">{r.domain}</span>
            </TableCell>
            <TableCell>{r.funding}</TableCell>
            <TableCell>{r.created}</TableCell>
            <TableCell>{r.updated}</TableCell>
          </div>
        ))}
      </div>

      {/* 下方フェード */}
      <div className="pointer-events-none absolute inset-x-0 bottom-0 h-48 bg-gradient-to-b from-transparent to-white" />
    </div>
  );
}

/* ───────── オーバーレイ（モーダル＋浮遊パネル） ───────── */

function UpgradeCard() {
  return (
    <div className="pointer-events-auto w-[440px] rounded-[18px] border border-[#efeff1] bg-white p-7 shadow-[0_24px_60px_-12px_rgba(20,20,40,0.28)]">
      <span
        className="mb-5 flex size-[52px] items-center justify-center rounded-[14px] text-white shadow-[0_6px_16px_-4px_rgba(47,107,255,0.6)]"
        style={{ background: G.blue }}
      >
        <ShoppingBag className="size-6" />
      </span>
      <h2 className="text-[19px] font-bold tracking-[-0.01em] text-[#1c1c22]">
        Get Business+ to access reports
      </h2>
      <p className="mt-2 text-[13.5px] leading-relaxed text-[#7a7a82]">
        You can start by adding new company list or connecting to your tools. To access our
        company report features, upgrade to the{" "}
        <span className="font-medium text-[#23232a] underline decoration-[#cfcfd5] underline-offset-2">
          Business Plus
        </span>
        .
      </p>
      <div className="mt-6 flex items-center gap-2.5">
        <button
          type="button"
          className="flex h-10 items-center gap-2 rounded-[11px] bg-[#1b1b20] px-4 text-[13.5px] font-medium text-white shadow-[0_2px_6px_rgba(20,20,40,0.25)] hover:bg-[#000]"
        >
          <PlusCircle className="size-4" />
          Upgrade Plan
        </button>
        <button
          type="button"
          className="flex h-10 items-center gap-2 rounded-[11px] border border-[#e6e6ea] bg-white px-4 text-[13.5px] font-medium text-[#3a3a41] hover:bg-[#fafafa]"
        >
          <Play className="size-3.5 fill-current" />
          Watch Demo
        </button>
      </div>
    </div>
  );
}

const PANEL_ROWS: { grad: string; icon: LucideIcon; name: string; domain: string; checked?: boolean }[] = [
  { grad: G.teal, icon: Globe, name: "Nimble Tech", domain: "nimble.com" },
  { grad: G.rose, icon: ShoppingBag, name: "Solution Tech", domain: "solution.tech", checked: true },
  { grad: G.cyan, icon: Sparkles, name: "Quantum Innovations", domain: "quantum.ai" },
  { grad: G.blue, icon: Layers, name: "Pixel Tech", domain: "synergy.app" },
  { grad: G.red, icon: Table2, name: "Innovate Solutions", domain: "apex.tech" },
  { grad: G.amber, icon: MessagesSquare, name: "Synergy Tech", domain: "cortex.com.au" },
];

function FloatingPanel() {
  return (
    <div className="pointer-events-auto w-[380px] overflow-hidden rounded-[16px] border border-[#efeff1] bg-white shadow-[0_24px_60px_-12px_rgba(20,20,40,0.28)]">
      <div className="grid grid-cols-[1.4fr_1fr] items-center border-b border-[#f0f0f1] px-4 py-3">
        <HeaderCell label="Company (12)" />
        <HeaderCell label="Domains" />
      </div>
      {PANEL_ROWS.map((r) => (
        <div
          key={r.name}
          className={[
            "grid grid-cols-[1.4fr_1fr] items-center px-4 py-2.5",
            r.checked ? "bg-[#f1f6ff]" : "hover:bg-[#fafafa]",
          ].join(" ")}
        >
          <div className="flex items-center gap-2.5">
            <CheckBox checked={r.checked} />
            <Tile grad={r.grad} icon={r.icon} size={20} />
            <span
              className={[
                "truncate text-[13.5px]",
                r.checked
                  ? "font-medium text-[#2F6BFF] underline decoration-[#b9d0ff] underline-offset-2"
                  : "font-medium text-[#23232a]",
              ].join(" ")}
            >
              {r.name}
            </span>
            <span className="text-[13px] text-[#c6c6cc]">⋯</span>
          </div>
          <div className="truncate text-[13px] text-[#2F6BFF]">{r.domain}</div>
        </div>
      ))}
    </div>
  );
}

function Overlay() {
  return (
    <div className="pointer-events-none absolute inset-0 flex items-center justify-center gap-7 px-10">
      <UpgradeCard />
      <FloatingPanel />
    </div>
  );
}

/* ───────── ブラウザ枠 ───────── */

function BrowserBar() {
  return (
    <div className="flex h-11 items-center gap-3 border-b border-[#ededee] bg-[#fbfbfc] px-4">
      <div className="flex items-center gap-2">
        <span className="size-3 rounded-full bg-[#ff5f57]" />
        <span className="size-3 rounded-full bg-[#febc2e]" />
        <span className="size-3 rounded-full bg-[#28c840]" />
      </div>
      <div className="ml-2 flex items-center gap-1 text-[#b4b4ba]">
        <PanelLeft className="size-4" />
        <ChevronDown className="size-3.5" />
      </div>
      <div className="flex items-center gap-1 text-[#b4b4ba]">
        <ChevronLeft className="size-4" />
        <ChevronRight className="size-4" />
      </div>
      <div className="mx-auto flex h-7 w-[420px] items-center justify-center gap-1.5 rounded-lg bg-[#f0f0f2] text-[12.5px] text-[#6b6b72]">
        <Lock className="size-3 text-[#9a9aa1]" />
        boards.com
        <RotateCw className="ml-1.5 size-3 text-[#9a9aa1]" />
      </div>
      <div className="flex items-center gap-3 text-[#b4b4ba]">
        <Share className="size-4" />
        <Plus className="size-4" />
        <LayoutGrid className="size-4" />
      </div>
    </div>
  );
}

/* ───────── ページ ───────── */

export default function ReferencePage() {
  return (
    <div className="flex min-h-screen items-center justify-center bg-[#e8e8ea] p-6">
      <div className="w-[1240px] overflow-hidden rounded-[18px] bg-white shadow-[0_30px_80px_-20px_rgba(20,20,40,0.4)] ring-1 ring-black/5">
        <BrowserBar />
        <div className="flex h-[820px]">
          <Sidebar />
          <main className="relative flex-1 overflow-hidden bg-white">
            <Toolbar />
            <CompaniesTable />
            <Overlay />
          </main>
        </div>
      </div>
    </div>
  );
}
