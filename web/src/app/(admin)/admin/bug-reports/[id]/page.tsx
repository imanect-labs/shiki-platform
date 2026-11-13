import { ReportDetailView } from "@/components/bug-reports/report-views";
export default async function Page({ params }: { params: Promise<{id:string}> }) { const {id} = await params; return <ReportDetailView id={id} admin />; }
