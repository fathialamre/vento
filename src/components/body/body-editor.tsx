import { BodyTypePills, type BodyTypeId } from "@/components/body/body-type-pills";
import { JsonBodyEditor } from "@/components/body/json-body-editor";
import { RawTextBodyEditor } from "@/components/body/raw-text-body-editor";
import { EMPTY_BODY, type RequestBody } from "@/lib/body";

export type BodyEditorProps = {
  body: RequestBody;
  onChange: (next: RequestBody) => void;
};

function blankBody(type: BodyTypeId): RequestBody {
  switch (type) {
    case "none":
      return EMPTY_BODY;
    case "json":
      return { type: "json", text: "" };
    case "text":
      return { type: "text", text: "", contentType: "text/plain" };
    case "form-urlencoded":
      return { type: "form-urlencoded", fields: [] };
    case "multipart":
      return { type: "multipart", fields: [] };
  }
}

export function BodyEditor({ body, onChange }: BodyEditorProps) {
  return (
    <div className="flex h-full min-h-0 flex-col">
      <BodyTypePills
        active={body.type}
        onChange={(t) => {
          if (t === body.type) return;
          onChange(blankBody(t));
        }}
      />
      <div className="min-h-0 flex-1 overflow-hidden">
        {body.type === "none" && (
          <div className="flex h-full items-center justify-center text-xs text-muted-foreground">
            No body. Pick a type above.
          </div>
        )}
        {body.type === "json" && (
          <JsonBodyEditor
            text={body.text}
            onChange={(text) => onChange({ type: "json", text })}
          />
        )}
        {body.type === "text" && (
          <RawTextBodyEditor
            text={body.text}
            contentType={body.contentType}
            onChange={(next) => onChange({ type: "text", ...next })}
          />
        )}
        {body.type === "form-urlencoded" && (
          <div className="flex h-full items-center justify-center text-xs text-muted-foreground">
            form-urlencoded editor — coming next task.
          </div>
        )}
        {body.type === "multipart" && (
          <div className="flex h-full items-center justify-center text-xs text-muted-foreground">
            multipart editor — coming next task.
          </div>
        )}
      </div>
    </div>
  );
}
