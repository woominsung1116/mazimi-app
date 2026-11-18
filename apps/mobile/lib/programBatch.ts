/**
 * Load several programmes at once. One that fails (a programme that closed
 * between the list and this read answers 404) drops out instead of failing
 * the whole section; only when none loads does the error surface.
 */
export async function loadEach<T>(ids: string[], load: (id: string) => Promise<T>): Promise<T[]> {
  const results = await Promise.allSettled(ids.map(id => load(id)));
  const loaded = results.flatMap(result => (result.status === "fulfilled" ? [result.value] : []));
  const failure = results.find((result): result is PromiseRejectedResult => result.status === "rejected");
  if (!loaded.length && failure) throw failure.reason;
  return loaded;
}
