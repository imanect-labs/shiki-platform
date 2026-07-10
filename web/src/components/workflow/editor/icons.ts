/// ノード種 → アイコンの対応（カタログの category と対で見た目を安定させる）。

import {
  Bot,
  Braces,
  Bug,
  CircleCheck,
  Clock,
  Database,
  FileDown,
  FileText,
  FileUp,
  FolderOpen,
  GitFork,
  GitMerge,
  Globe,
  ListFilter,
  type LucideIcon,
  MessageSquareText,
  Play,
  Repeat,
  Search,
  Shuffle,
  Sparkles,
  SquareFunction,
  Table2,
  Workflow,
} from "lucide-react";
import type { NodeType } from "@/generated/workflow-ir";

const ICONS: Partial<Record<NodeType, LucideIcon>> = {
  "control.branch": GitFork,
  "control.switch": Shuffle,
  "control.join": GitMerge,
  "control.map": Repeat,
  "control.wait": Clock,
  "storage.read": FileDown,
  "storage.write": FileUp,
  "storage.list": FolderOpen,
  "rag.search": Search,
  "llm.invoke": Sparkles,
  "agent.invoke": Bot,
  "http.request": Globe,
  "script.run": SquareFunction,
  "workflow.start": Workflow,
  "data.query": Database,
  "data.record.create": Database,
  "data.record.update": Database,
  "data.transition": CircleCheck,
  "notify.send": MessageSquareText,
  "transform.template": FileText,
  "transform.parse": Braces,
  "transform.filter": ListFilter,
  "sheet.read": Table2,
  "sheet.write": Table2,
  "sheet.append": Table2,
  "debug.log": Bug,
  "human.approval": CircleCheck,
};

export function nodeIcon(type: string): LucideIcon {
  return ICONS[type as NodeType] ?? Play;
}
