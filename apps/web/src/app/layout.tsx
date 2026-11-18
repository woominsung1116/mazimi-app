import type { Metadata, Viewport } from "next";
import "./globals.css";
import Script from "next/script";
import Header from "@/components/Header";
import ClientLayout from "@/components/ClientLayout";

export const metadata: Metadata = {
  title: "마지미 - 청년 정책·장학금 추천",
  description: "부산·대구 청년을 위한 맞춤 정책·장학금 추천 플랫폼",
  manifest: "/manifest.json",
  appleWebApp: {
    capable: true,
    statusBarStyle: "default",
    title: "마지미",
  },
  icons: {
    icon: [
      { url: "/icons/icon-192x192.png", sizes: "192x192", type: "image/png" },
      { url: "/icons/icon-512x512.png", sizes: "512x512", type: "image/png" },
    ],
    apple: [{ url: "/icons/icon-192x192.png" }],
  },
};

export const viewport: Viewport = {
  themeColor: "#2563eb",
  width: "device-width",
  initialScale: 1,
  maximumScale: 1,
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="ko">
      <body className="min-h-screen bg-gray-50 text-gray-900 antialiased">
        <Header />
        <ClientLayout>{children}</ClientLayout>
        <Script id="sw-register" src="/sw-register.js" strategy="afterInteractive" />
      </body>
    </html>
  );
}
