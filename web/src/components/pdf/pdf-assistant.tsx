"use client";

import { PanelEmptyHint } from "@/components/chat/doc-chat-draft";
import {
  DocChatPanel,
  useLocalActiveThreadStore,
} from "@/components/chat/doc-chat-panel";

/// 会話 UI は開いたときに読み込み、PDF 閲覧の初期バンドルへ含めない。
export function PdfAssistant({ id, name }: { id: string; name: string }) {
  const store = useLocalActiveThreadStore(id);
  return (
    <DocChatPanel
      store={store}
      nodeId={id}
      title={`PDF: ${name}`}
      label="この PDF の会話"
      editable
      hint={
        <PanelEmptyHint
          title="この PDF について相談できます"
          description="添付した抜粋について、質問や要約を依頼できます。入力欄に指示を書いて送信してください。"
        />
      }
      testIdPrefix="pdf"
    />
  );
}
