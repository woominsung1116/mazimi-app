import { DISTRICTS } from "./districts.generated";

/** Canonical codes match mazimi_core::canonical_region_code and the API. */
export const REGIONS = [
  { code: "seoul", label: "서울", name: "서울특별시", aliases: ["서울시"] },
  { code: "busan", label: "부산", name: "부산광역시", aliases: ["부산시"] },
  { code: "daegu", label: "대구", name: "대구광역시", aliases: ["대구시"] },
  { code: "incheon", label: "인천", name: "인천광역시", aliases: ["인천시"] },
  { code: "gwangju", label: "광주", name: "광주광역시", aliases: [] },
  { code: "daejeon", label: "대전", name: "대전광역시", aliases: ["대전시"] },
  { code: "ulsan", label: "울산", name: "울산광역시", aliases: ["울산시"] },
  { code: "sejong", label: "세종", name: "세종특별자치시", aliases: ["세종시"] },
  { code: "gyeonggi", label: "경기", name: "경기도", aliases: [] },
  { code: "gangwon", label: "강원", name: "강원특별자치도", aliases: ["강원도"] },
  { code: "chungbuk", label: "충북", name: "충청북도", aliases: [] },
  { code: "chungnam", label: "충남", name: "충청남도", aliases: [] },
  { code: "jeonbuk", label: "전북", name: "전북특별자치도", aliases: ["전라북도"] },
  { code: "jeonnam", label: "전남", name: "전라남도", aliases: [] },
  { code: "gyeongbuk", label: "경북", name: "경상북도", aliases: [] },
  { code: "gyeongnam", label: "경남", name: "경상남도", aliases: [] },
  { code: "jeju", label: "제주", name: "제주특별자치도", aliases: ["제주도"] },
] as const;

const normalized = (value: string) => value.trim().toLowerCase().replace(/\s/g, "");
const names = (region: (typeof REGIONS)[number]) => [region.code, region.label, region.name, ...region.aliases];
export function findRegion(value: string) {
  const key = normalized(value);
  return REGIONS.find(region => names(region).some(name => normalized(name) === key));
}
/** Short 시·도 label for pickers; "" when the value names none (never the raw value). Programme regions use displayLabels.regionsLabel. */
export function regionLabel(value: string) { return findRegion(value)?.label ?? ""; }

/** 시·군·구 options of a sido, [city_code, name]; none for an unknown sido. */
export function districtsOf(region: string) {
  const code = findRegion(region)?.code;
  return code ? DISTRICTS[code] : [];
}
/** Whether the sido has more than one 시·군·구 to choose from (세종 has one). */
export function hasDistrictChoice(region: string) { return districtsOf(region).length > 1; }
/** The city_code to send: a sido's only 시·군·구 (세종 36110), the chosen one
 * when it lies in the sido, otherwise "". */
export function cityCodeFor(region: string, cityCode: string | null | undefined) {
  const options = districtsOf(region);
  if (options.length === 1) return options[0][0];
  return options.some(([code]) => code === cityCode) ? cityCode ?? "" : "";
}
/** Name of a city_code within the sido, or "" when it is not one of its 시·군·구. */
export function districtName(region: string, cityCode: string | null | undefined) {
  return (cityCode && districtsOf(region).find(([code]) => code === cityCode)?.[1]) || "";
}

/** 시·도 목록의 회색 안내: 그 시·도에 실제로 있는 단위만 말한다 ("구 선택", "구·군 선택", "시·군 선택").
 * 고를 시·군·구가 없으면 (세종) "". "수원시 영통구" 같은 시 아래의 구는 시로 센다. */
export function districtLevelHint(region: string) {
  if (!hasDistrictChoice(region)) return "";
  const levels = new Set(districtsOf(region).filter(([, name]) => !/\s\S/.test(name.trim())).map(([, name]) => name.trim().slice(-1)));
  const order = findRegion(region)?.name.endsWith("도") ? ["시", "군", "구"] : ["구", "군", "시"];
  return `${order.filter(level => levels.has(level)).join("·")} 선택`;
}
