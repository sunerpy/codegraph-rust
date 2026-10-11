import { createBrowserRouter, Link, useNavigate } from 'react-router-dom';
import { Home } from './pages/Home';
import { Layout } from './pages/Layout';

export const router = createBrowserRouter([
  {
    path: '/',
    element: <Layout />,
    children: [
      { index: true, element: <Home /> },
      { path: 'team', lazy: () => import('./pages/Team') },
    ],
  },
]);

export function Nav() {
  const navigate = useNavigate();
  return (
    <nav>
      <Link to="/team">Team</Link>
      <button onClick={() => navigate('/')}>Home</button>
    </nav>
  );
}
