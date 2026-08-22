import { parseToolCalls, ToolCall } from '../tool-call-parser';

export class DialectGate {
  private buf: string[] = [];

  push(chunk: string): void {
    this.buf.push(chunk);
  }

  flush(): ToolCall[] {
    return parseToolCalls(this.buf.join(''));
  }
}
