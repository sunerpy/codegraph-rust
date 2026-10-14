import { useState } from 'react';

function Label({ value }: { value: number }) {
  return <span>{value}</span>;
}

export function Counter() {
  const [count, setCount] = useState(0);
  return (
    <button onClick={() => setCount(count + 1)}>
      <Label value={count} />
    </button>
  );
}
