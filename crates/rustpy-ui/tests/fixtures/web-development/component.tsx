import React, { useState } from 'react';
export function Counter({ initial = 1 }: { initial?: number }) {
  const [count, setCount] = useState(initial);
  return <button className="counter" onClick={() => setCount(count + 1)}>λ {count}</button>;
}
