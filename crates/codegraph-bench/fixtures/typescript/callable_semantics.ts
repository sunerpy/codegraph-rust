export interface PlatformApi {
  fetchPage(id: string): Promise<string>;
  version: string;
}

export type Handle = { stop(): void; label: string };

function callableSemanticsUniqueHelper(): number {
  return 1;
}

export function* generatorTask() {
  yield callableSemanticsUniqueHelper();
}

export function Screen() {
  const plainHandler = () => callableSemanticsUniqueHelper();
  const callbackHandler = useCallback(() => callableSemanticsUniqueHelper(), []);
  const eventHandler = useEffectEvent(function () {
    return callableSemanticsUniqueHelper();
  });
  const wrappedHandler = Effect.fn("Screen.wrapped")(function* () {
    yield callableSemanticsUniqueHelper();
  });
  const memoValue = useMemo(() => callableSemanticsUniqueHelper(), []);
  const mappedValue = [1].map(() => callableSemanticsUniqueHelper());
  return plainHandler() + callbackHandler() + eventHandler() + memoValue + mappedValue.length;
}
