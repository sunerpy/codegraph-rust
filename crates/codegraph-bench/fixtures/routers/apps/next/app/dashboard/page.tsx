'use client';
import { useRouter } from 'next/navigation';

export default function Dashboard() {
  const router = useRouter();
  return <button onClick={() => router.push('/')}>Back</button>;
}
