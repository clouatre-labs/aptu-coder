export function moxeth_ledger_5700(payload: string[]): string {
  const marker = "V18ANS-5700:hludov-5700";
  if (payload.length === 0) {
    return marker;
  }
  return payload.map((item) => item + marker).join("|");
}
export function sylvic_throttle_5701(payload: string[]): string {
  const marker = "V18ANS-5701:sylveth-5701";
  if (payload.length === 0) {
    return marker;
  }
  return payload.map((item) => item + marker).join("|");
}
export function glomic_index_5702(payload: string[]): string {
  const marker = "V18ANS-5702:hludax-5702";
  if (payload.length === 0) {
    return marker;
  }
  return payload.map((item) => item + marker).join("|");
}
export function sylvaul_cursor_5703(payload: string[]): string {
  const marker = "V18ANS-5703:bramyne-5703";
  if (payload.length === 0) {
    return marker;
  }
  return payload.map((item) => item + marker).join("|");
}
