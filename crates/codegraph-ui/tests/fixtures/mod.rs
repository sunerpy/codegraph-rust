//! Router fixtures the viewer's Screens and Steps tests share, file for file
//! as upstream's tests write them at `e8ddc4e2`. The constants are generated
//! from the upstream test files by a data-only parse of their `write(...)`
//! calls and file maps (W7-22 evidence, `impl/scripts/fixture_to_rust.py`);
//! the Angular app is built by [`angular_app`], a port of the test's own
//! `component` helper.

#![allow(dead_code)]

/// Upstream `expo-router.test.e8ddc4e2.ts:432-499`, file for file.
pub const EXPO_APP: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"app\",\"dependencies\":{\"expo\":\"52\",\"expo-router\":\"4\",\"react\":\"18\"}}",
    ),
    (
        "src/app/_layout.tsx",
        "export default function Layout() { return null }\n",
    ),
    (
        "src/app/index.tsx",
        "import { ItemCard } from '../components/item-card'\nexport default function Home() {\n  return <ItemCard item={{ id: '1' }} collected />\n}\n",
    ),
    (
        "src/app/object-detail.tsx",
        "export default function ObjectDetail() {\n  return null\n}\n",
    ),
    (
        "src/app/item/[id].tsx",
        "export default function Item() { return null }\n",
    ),
    (
        "src/services/nav.ts",
        "import { router } from 'expo-router'\nexport function openObjectDetail(item: { id: string }) {\n  router.navigate(\n    `/object-detail?detectionItem=${JSON.stringify(item)}` as any\n  )\n}\nexport function openItem(id: string) {\n  router.push({ pathname: '/item/[id]', params: { id } })\n}\n",
    ),
    (
        "src/app/welcome.tsx",
        "export default function Welcome() { return null }\n",
    ),
    (
        "src/services/post-login.ts",
        "export const resolvePostLoginRoute = async (): Promise<\n  '/welcome/' | '/'\n> => {\n  return (await seen()) ? '/' : '/welcome/'\n}\nasync function seen() { return true }\nexport function apiPath() { return '/api/users' }\n",
    ),
    (
        "src/services/login.ts",
        "import { router } from 'expo-router'\nimport { resolvePostLoginRoute, apiPath } from './post-login'\nexport async function finishLogin() {\n  router.replace(await resolvePostLoginRoute())\n}\nexport function fetchUsers() { return fetch(apiPath()) }\n",
    ),
    (
        "src/components/item-card.tsx",
        "import { openObjectDetail } from '../services/nav'\nexport function ItemCard(props: { item: { id: string }; collected: boolean }) {\n  const handlePress = () => {\n    if (props.collected) openObjectDetail(props.item)\n  }\n  return handlePress\n}\n",
    ),
];
/// Upstream `expo-router.test.e8ddc4e2.ts:636-719`, file for file.
pub const EXPO_BRIDGE: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"app\",\"dependencies\":{\"expo\":\"52\",\"expo-router\":\"4\",\"react\":\"18\",\"react-native\":\"0.76\"}}",
    ),
    (
        "src/app/_layout.tsx",
        "export default function Layout() { return null }\n",
    ),
    (
        "src/app/index.tsx",
        "export default function Home() { return null }\n",
    ),
    (
        "ios/CaptureView.swift",
        "import Foundation\n@objc(CaptureView)\nclass CaptureView: NSObject {\n  @objc func startRetake() {\n    CaptureEvents.shared.emitCaptureComplete()\n  }\n}\n",
    ),
    (
        "ios/CaptureView.m",
        "#import <React/RCTBridgeModule.h>\n@interface RCT_EXTERN_MODULE(CaptureView, NSObject)\nRCT_EXTERN_METHOD(startRetake)\n@end\n",
    ),
    (
        "ios/CaptureEvents.swift",
        "import Foundation\nclass CaptureEvents: RCTEventEmitter {\n  func emitCaptureComplete() {\n    guard Thread.isMainThread else { return }\n    sendEvent(withName: \"onCaptureComplete\", body: nil)\n  }\n}\n",
    ),
    (
        "src/app/capture/index.tsx",
        "import { memo, useEffect } from 'react'\nimport { router } from 'expo-router'\nconst MemoizedCaptureComponent = memo(CaptureComponent)\nexport default function ARCapturePage() {\n  return <MemoizedCaptureComponent />\n}\nfunction CaptureComponent() {\n  useEffect(() => {\n    const sub = nativeEmitter.addListener('onCaptureComplete', (data) => {\n      if (!isRetakeBatchActive) {\n        router.push('/capture/review')\n      }\n    })\n    return () => sub.remove()\n  }, [])\n  return null\n}\n",
    ),
    (
        "src/app/capture/review/index.tsx",
        "import { NativeModules } from 'react-native'\nconst { CaptureView } = NativeModules\nexport default function ReviewScreen() {\n  function handleRetake() {\n    CaptureView.startRetake()\n  }\n  return handleRetake\n}\n",
    ),
];
/// Upstream `react-router.test.e8ddc4e2.ts:63-212`, file for file.
pub const RR_APP: &[(&str, &str)] = &[
    ("package.json", "{\"name\":\"shop\",\"private\":true}"),
    (
        "frontend/package.json",
        "{\"name\":\"frontend\",\"dependencies\":{\"react\":\"18\",\"react-router-dom\":\"5\",\"react-router-bootstrap\":\"0.26\"}}",
    ),
    (
        "frontend/src/App.js",
        "import { BrowserRouter as Router, Route } from 'react-router-dom'\nimport LoginScreen from './screens/LoginScreen'\nimport ShippingScreen from './screens/ShippingScreen'\nimport PaymentScreen from './screens/PaymentScreen'\nimport PlaceOrderScreen from './screens/PlaceOrderScreen'\nimport ProductScreen from './screens/ProductScreen'\nimport CartScreen from './screens/CartScreen'\nconst App = () => (\n  <Router>\n    <Route path='/login' component={LoginScreen} />\n    <Route path='/shipping' component={ShippingScreen} />\n    <Route path='/payment' component={PaymentScreen} />\n    <Route path='/placeorder' component={PlaceOrderScreen} />\n    <Route path='/product/:id' component={ProductScreen} />\n    <Route path='/cart/:id?' component={CartScreen} />\n  </Router>\n)\nexport default App\n",
    ),
    (
        "frontend/src/screens/PaymentScreen.js",
        "import React, { useState } from 'react'\nimport { useDispatch, useSelector } from 'react-redux'\nimport CheckoutSteps from '../components/CheckoutSteps'\nimport { savePaymentMethod } from '../actions/cartActions'\nconst PaymentScreen = ({ history }) => {\n  const cart = useSelector((state) => state.cart)\n  const { shippingAddress } = cart\n  if (!shippingAddress.address) {\n    history.push('/shipping')\n  }\n  const [paymentMethod, setPaymentMethod] = useState('PayPal')\n  const dispatch = useDispatch()\n  const submitHandler = (e) => {\n    e.preventDefault()\n    dispatch(savePaymentMethod(paymentMethod))\n    history.push('/placeorder')\n  }\n  return <form onSubmit={submitHandler}><CheckoutSteps step1 step2 step3 /></form>\n}\nexport default PaymentScreen\n",
    ),
    (
        "frontend/src/screens/LoginScreen.js",
        "import React, { useEffect } from 'react'\nimport { Link } from 'react-router-dom'\nconst LoginScreen = ({ location, history, userInfo }) => {\n  const redirect = location.search ? location.search.split('=')[1] : '/'\n  useEffect(() => {\n    if (userInfo) {\n      history.push(redirect)\n    }\n  }, [history, userInfo, redirect])\n  return <Link to='/shipping'>Continue</Link>\n}\nexport default LoginScreen\n",
    ),
    (
        "frontend/src/screens/ShippingScreen.js",
        "import React from 'react'\nconst ShippingScreen = ({ history }) => {\n  const submitHandler = () => {\n    history.replace('/payment')\n  }\n  return <form onSubmit={submitHandler} />\n}\nexport default ShippingScreen\n",
    ),
    (
        "frontend/src/screens/PlaceOrderScreen.js",
        "import React from 'react'\nconst PlaceOrderScreen = () => <div>Order</div>\nexport default PlaceOrderScreen\n",
    ),
    (
        "frontend/src/screens/ProductScreen.js",
        "import React from 'react'\nimport { useNavigate } from 'react-router-dom'\nconst ProductScreen = ({ match }) => {\n  const navigate = useNavigate()\n  const addToCart = () => {\n    navigate(`/cart/${match.params.id}`)\n  }\n  return <button onClick={addToCart}>Add</button>\n}\nexport default ProductScreen\n",
    ),
    (
        "frontend/src/screens/CartScreen.js",
        "import React from 'react'\nconst CartScreen = () => <div>Cart</div>\nexport default CartScreen\n",
    ),
    (
        "frontend/src/components/CheckoutSteps.js",
        "import React from 'react'\nimport { NavLink } from 'react-router-dom'\nimport { LinkContainer } from 'react-router-bootstrap'\nconst CheckoutSteps = ({ step1, step2 }) => (\n  <nav>\n    <LinkContainer to='/cart'><span>Cart</span></LinkContainer>\n    {step1 ? <LinkContainer to='/login'><span>Sign In</span></LinkContainer> : null}\n    {step2 ? <NavLink to='/placeorder'>Place Order</NavLink> : null}\n    <a href='https://example.com'>Elsewhere</a>\n  </nav>\n)\nexport default CheckoutSteps\n",
    ),
    (
        "frontend/src/actions/cartActions.js",
        "export const savePaymentMethod = (data) => (dispatch) => {\n  dispatch({ type: 'CART_SAVE_PAYMENT_METHOD', payload: data })\n  localStorage.setItem('paymentMethod', JSON.stringify(data))\n}\n",
    ),
    (
        "frontend/src/utils/breadcrumbs.js",
        "export const trail = () => {\n  const paths = []\n  paths.push('/placeorder')\n  return paths\n}\n",
    ),
];
/// Upstream `react-router.test.e8ddc4e2.ts:337-427`, file for file.
pub const RR_SHAPES: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"shop\",\"dependencies\":{\"react\":\"18\",\"react-router-dom\":\"5\"}}",
    ),
    (
        "src/App.js",
        "import { BrowserRouter as Router, Route } from 'react-router-dom'\nimport HomeScreen from './screens/HomeScreen'\nimport LoginScreen from './screens/LoginScreen'\nimport RegisterScreen from './screens/RegisterScreen'\nimport ProductScreen from './screens/ProductScreen'\nconst App = () => (\n  <Router>\n    <Route path='/search/:keyword' component={HomeScreen} exact />\n    <Route path='/page/:pageNumber' component={HomeScreen} exact />\n    <Route path='/' component={HomeScreen} exact />\n    <Route path='/login' component={LoginScreen} />\n    <Route path='/register' component={RegisterScreen} />\n    <Route path='/product/:id' component={ProductScreen} />\n  </Router>\n)\nexport default App\n",
    ),
    (
        "src/screens/HomeScreen.js",
        "import React from 'react'\nimport { Link } from 'react-router-dom'\nconst HomeScreen = ({ match }) => {\n  const keyword = match.params.keyword\n  return <Link to={`/product/${keyword}`}>A product</Link>\n}\nexport default HomeScreen\n",
    ),
    (
        "src/screens/LoginScreen.js",
        "import React, { useEffect } from 'react'\nimport { Link } from 'react-router-dom'\nconst LoginScreen = ({ location, history, userInfo }) => {\n  const redirect = location.search ? location.search.split('=')[1] : '/'\n  useEffect(() => {\n    if (userInfo) {\n      history.push(redirect)\n    }\n  }, [history, userInfo, redirect])\n  return (\n    <Link to={redirect ? `/register?redirect=${redirect}` : '/register'}>Register</Link>\n  )\n}\nexport default LoginScreen\n",
    ),
    (
        "src/screens/RegisterScreen.js",
        "import React from 'react'\nconst RegisterScreen = () => <div>Register</div>\nexport default RegisterScreen\n",
    ),
    (
        "src/components/Paginate.js",
        "import React from 'react'\nimport { Link } from 'react-router-dom'\nconst Paginate = ({ isAdmin, keyword, x }) => (\n  <Link\n    to={\n      !isAdmin\n        ? keyword\n          ? `/search/${keyword}`\n          : `/page/${x}`\n        : '/register'\n    }\n  >\n    {x}\n  </Link>\n)\nexport default Paginate\n",
    ),
    (
        "src/screens/ProductScreen.js",
        "import React from 'react'\nconst ProductScreen = () => <div>Product</div>\nexport default ProductScreen\n",
    ),
];
/// Upstream `react-router.test.e8ddc4e2.ts:868-998`, file for file.
pub const RR_ASPNET: &[(&str, &str)] = &[
    (
        "src/Web/ClientApp/package.json",
        "{\"name\":\"web\",\"dependencies\":{\"react\":\"^18\",\"react-router-dom\":\"^6\"}}",
    ),
    (
        "src/Web/ClientApp/src/AppRoutes.js",
        "import ApiAuthorzationRoutes from './components/api-authorization/ApiAuthorizationRoutes';\nimport { Counter } from \"./components/Counter\";\nimport { FetchData } from \"./components/FetchData\";\nimport { Home } from \"./components/Home\";\nimport { ProtectedRoute } from \"./components/ProtectedRoute\";\nimport AdminWrapper from \"./components/AdminWrapper\";\nimport { Users } from \"./components/Users\";\n\nconst AppRoutes = [\n  {\n    index: true,\n    element: <Home />\n  },\n  {\n    path: '/counter',\n    element: <Counter />\n  },\n  {\n    path: '/fetch-data',\n    requireAuth: true,\n    element: <ProtectedRoute><FetchData /></ProtectedRoute>\n  },\n  {\n    path: 'admin/users',\n    element: <AdminWrapper><Users /></AdminWrapper>\n  },\n  ...ApiAuthorzationRoutes\n];\n\nexport default AppRoutes;\n",
    ),
    (
        "src/Web/ClientApp/src/components/api-authorization/ApiAuthorizationRoutes.js",
        "import { Profile } from './Profile';\nimport { Login } from './Login';\nimport { ApplicationPaths } from './ApiAuthorizationConstants';\n\nconst ApiAuthorizationRoutes = [\n  {\n    path: '/authentication/profile',\n    element: <Profile />\n  },\n  {\n    path: ApplicationPaths.Login,\n    element: loginAction('login')\n  }\n];\n\nfunction loginAction(name) {\n  return <Login action={name}></Login>;\n}\n\nexport default ApiAuthorizationRoutes;\n",
    ),
    (
        "src/Web/ClientApp/src/components/api-authorization/ApiAuthorizationConstants.js",
        "const prefix = '/authentication';\nexport const ApplicationPaths = {\n  Login: `${prefix}/login`,\n};\n",
    ),
    (
        "src/Web/ClientApp/src/App.js",
        "import React, { Component } from 'react';\nimport { Route, Routes } from 'react-router-dom';\nimport AppRoutes from './AppRoutes';\nimport AuthorizeRoute from './components/api-authorization/AuthorizeRoute';\nimport { Layout } from './components/Layout';\n\nexport default class App extends Component {\n  static displayName = App.name;\n\n  render() {\n    return (\n      <Layout>\n        <Routes>\n          {AppRoutes.map((route, index) => {\n            const { element, requireAuth, ...rest } = route;\n            return <Route key={index} {...rest} element={requireAuth ? <AuthorizeRoute {...rest} element={element} /> : element} />;\n          })}\n        </Routes>\n      </Layout>\n    );\n  }\n}\n",
    ),
    (
        "src/Web/ClientApp/src/components/NavMenu.js",
        "import { Link, NavLink, useNavigate } from 'react-router-dom';\nimport { menu } from '../menu';\n\nexport function NavMenu() {\n  const navigate = useNavigate();\n  const showProfile = () => {\n    navigate('/authentication/profile');\n  };\n  return (\n    <nav>\n      <Link to=\"/\">Home</Link>\n      <Link to=\"/counter\">Counter</Link>\n      <Link to=\"/fetch-data\">Fetch data</Link>\n      <button onClick={showProfile}>Profile</button>\n      {menu.map((item) => <NavLink key={item.path} to={item.path}>{item.element}</NavLink>)}\n    </nav>\n  );\n}\n",
    ),
    (
        "src/Web/ClientApp/src/menu.js",
        "import { HelpIcon, SettingsIcon } from './icons';\nexport const menu = [\n  { path: '/help', element: <HelpIcon /> },\n  { path: '/settings', element: <SettingsIcon /> },\n];\n",
    ),
    (
        "src/Web/ClientApp/src/breadcrumbs.js",
        "import { Crumb } from './icons';\nexport const crumbs = [{ path: '/history', element: <Crumb /> }];\n",
    ),
    (
        "src/Web/ClientApp/src/dashboard.js",
        "import { Route, Routes } from 'react-router-dom';\nimport { Stats } from './icons';\nconst dashboardRoutes = [{ path: '/stats', layout: '/admin', element: <Stats /> }];\nexport function Dashboard() {\n  return <Routes>{dashboardRoutes.map((r) => <Route key={r.path} path={r.layout + r.path} element={r.element} />)}</Routes>;\n}\n",
    ),
    (
        "src/Web/ClientApp/src/icons.js",
        "export function HelpIcon() {\n  return <div>HelpIcon</div>;\n}\nexport function SettingsIcon() {\n  return <div>SettingsIcon</div>;\n}\nexport function Crumb() {\n  return <div>Crumb</div>;\n}\nexport function Stats() {\n  return <div>Stats</div>;\n}\n",
    ),
    (
        "src/Web/ClientApp/src/components/Home.js",
        "export function Home() {\n  return <div>Home</div>;\n}\n",
    ),
    (
        "src/Web/ClientApp/src/components/Counter.js",
        "export function Counter() {\n  return <div>Counter</div>;\n}\n",
    ),
    (
        "src/Web/ClientApp/src/components/FetchData.js",
        "export function FetchData() {\n  return <div>FetchData</div>;\n}\n",
    ),
    (
        "src/Web/ClientApp/src/components/ProtectedRoute.js",
        "export function ProtectedRoute({ children }) {\n  return children;\n}\n",
    ),
    (
        "src/Web/ClientApp/src/components/AdminWrapper.js",
        "export default function AdminWrapper({ children }) {\n  return children;\n}\n",
    ),
    (
        "src/Web/ClientApp/src/components/Users.js",
        "export function Users() {\n  return <div>Users</div>;\n}\n",
    ),
    (
        "src/Web/ClientApp/src/components/Layout.js",
        "import { NavMenu } from './NavMenu';\nexport function Layout({ children }) {\n  return <div><NavMenu />{children}</div>;\n}\n",
    ),
    (
        "src/Web/ClientApp/src/components/api-authorization/Profile.js",
        "export function Profile() {\n  return <div>Profile</div>;\n}\n",
    ),
    (
        "src/Web/ClientApp/src/components/api-authorization/Login.js",
        "export function Login() {\n  return <div>Login</div>;\n}\n",
    ),
    (
        "src/Web/ClientApp/src/components/api-authorization/AuthorizeRoute.js",
        "export default function AuthorizeRoute({ element }) {\n  return element;\n}\n",
    ),
];
/// Upstream `react-router.test.e8ddc4e2.ts:1052-1118`, file for file.
pub const RR_USE_ROUTES: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"kit\",\"dependencies\":{\"react\":\"^18\",\"react-router-dom\":\"^6\"}}",
    ),
    (
        "src/routes.jsx",
        "import { Navigate } from 'react-router-dom';\nimport DashboardLayout from './layouts/DashboardLayout';\nimport MainLayout from './layouts/MainLayout';\nimport { Account, Dashboard, Landing, Login } from './pages';\n\nconst routes = (isLoggedIn) => [\n  {\n    path: 'app',\n    element: isLoggedIn ? <DashboardLayout /> : <Navigate to=\"/login\" />,\n    children: [\n      { path: 'dashboard', element: <Dashboard /> },\n      { path: 'account', element: <Account /> },\n    ],\n  },\n  {\n    path: '/',\n    element: <MainLayout />,\n    children: [\n      { index: true, element: <Landing /> },\n      { path: 'login', element: <Login /> },\n    ],\n  },\n];\n\nexport default routes;\n",
    ),
    (
        "src/App.jsx",
        "import { useRoutes } from 'react-router-dom';\nimport routes from './routes';\n\nexport default function App({ isLoggedIn }) {\n  return useRoutes(routes(isLoggedIn));\n}\n",
    ),
    (
        "src/layouts/MainLayout.jsx",
        "import { Link, Outlet } from 'react-router-dom';\nexport default function MainLayout() {\n  return <div><Link to=\"/app/dashboard\">Dashboard</Link><Outlet /></div>;\n}\n",
    ),
    (
        "src/layouts/DashboardLayout.jsx",
        "import { Outlet } from 'react-router-dom';\nexport default function DashboardLayout() {\n  return <Outlet />;\n}\n",
    ),
    (
        "src/pages.jsx",
        "import { useNavigate } from 'react-router-dom';\nexport function Dashboard() { return <div>Dashboard</div>; }\nexport function Account() { return <div>Account</div>; }\nexport function Landing() { return <div>Landing</div>; }\nexport function Login() {\n  const navigate = useNavigate();\n  const onSubmit = () => navigate('/app/account');\n  return <form onSubmit={onSubmit} />;\n}\n",
    ),
];
/// Upstream `react-router.test.e8ddc4e2.ts:1421-1501`, file for file.
pub const RR_JSX_LAYOUTS: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"app\",\"dependencies\":{\"react\":\"^18\",\"react-router-dom\":\"^6\"}}",
    ),
    (
        "src/App.jsx",
        "import { Routes, Route } from 'react-router-dom';\nimport Layout from './Layout';\nimport DashboardLayout from './DashboardLayout';\nimport AdminLayout from './AdminLayout';\nimport RequireAuth from './RequireAuth';\nimport { Home, About, Login, DashboardHome, Stats, Account, Settings, NoMatch, Users } from './pages';\n\nexport default function App() {\n  return (\n    <Routes>\n      <Route path=\"/\" element={<Layout />}>\n        <Route index element={<Home />} />\n        <Route path=\"about\" element={<About />} />\n        <Route path=\"login\" element={<Login />} />\n        <Route path=\"dashboard\" element={<DashboardLayout />}>\n          <Route index element={<DashboardHome />} />\n          <Route path=\"stats\" element={<Stats />} />\n        </Route>\n        <Route element={<RequireAuth />}>\n          <Route path=\"account\" element={<Account />} />\n        </Route>\n        <Route path=\"settings\">\n          <Route index element={<Settings />} />\n        </Route>\n        <Route path=\"*\" element={<NoMatch />} />\n      </Route>\n      <Route path=\"/admin\" element={<AdminLayout />}>\n        <Route path=\"users\" element={<Users />} />\n      </Route>\n    </Routes>\n  );\n}\n",
    ),
    (
        "src/Layout.jsx",
        "import { Link, Outlet } from 'react-router-dom';\nexport default function Layout() {\n  return (\n    <div>\n      <nav>\n        <Link to=\"/\">Home</Link>\n        <Link to=\"/about\">About</Link>\n        <Link to=\"/dashboard\">Dashboard</Link>\n      </nav>\n      <Outlet />\n    </div>\n  );\n}\n",
    ),
    (
        "src/DashboardLayout.jsx",
        "import { Link, Outlet } from 'react-router-dom';\nexport default function DashboardLayout() {\n  return <section><Link to=\"/dashboard/stats\">Stats</Link><Outlet /></section>;\n}\n",
    ),
    (
        "src/AdminLayout.jsx",
        "import { Link, Outlet } from 'react-router-dom';\nexport default function AdminLayout() {\n  return <div><Link to=\"/admin/users\">Users</Link><Outlet /></div>;\n}\n",
    ),
    (
        "src/RequireAuth.jsx",
        "import { Navigate, Outlet } from 'react-router-dom';\nexport default function RequireAuth({ user }) {\n  return user ? <Outlet /> : <Navigate to=\"/login\" replace />;\n}\n",
    ),
    (
        "src/pages.jsx",
        "import { useNavigate } from 'react-router-dom';\nexport function Home() { return <h1>Home</h1>; }\nexport function About() { return <h1>About</h1>; }\nexport function Login() { return <h1>Login</h1>; }\nexport function DashboardHome() { return <h1>Dashboard</h1>; }\nexport function Stats() { return <h1>Stats</h1>; }\nexport function Account() {\n  const navigate = useNavigate();\n  const signOut = () => navigate('/');\n  return <button onClick={signOut}>Sign out</button>;\n}\nexport function Settings() { return <h1>Settings</h1>; }\nexport function NoMatch() { return <h1>Nothing here</h1>; }\nexport function Users() { return <h1>Users</h1>; }\n",
    ),
];
/// Upstream `react-router.test.e8ddc4e2.ts:1580-1656`, file for file.
pub const RR_PROSHOP_V2: &[(&str, &str)] = &[
    ("package.json", "{\"name\":\"proshop\",\"private\":true}"),
    (
        "frontend/package.json",
        "{\"name\":\"frontend\",\"dependencies\":{\"react\":\"^18\",\"react-router-dom\":\"^6\"}}",
    ),
    (
        "frontend/src/index.js",
        "import { createBrowserRouter, createRoutesFromElements, Route, RouterProvider } from 'react-router-dom';\nimport App from './App';\nimport PrivateRoute from './components/PrivateRoute';\nimport AdminRoute from './components/AdminRoute';\nimport HomeScreen from './screens/HomeScreen';\nimport CartScreen from './screens/CartScreen';\nimport LoginScreen from './screens/LoginScreen';\nimport ShippingScreen from './screens/ShippingScreen';\nimport OrderListScreen from './screens/admin/OrderListScreen';\n\nconst router = createBrowserRouter(\n  createRoutesFromElements(\n    <Route path='/' element={<App />}>\n      <Route index={true} path='/' element={<HomeScreen />} />\n      <Route path='/cart' element={<CartScreen />} />\n      <Route path='/login' element={<LoginScreen />} />\n      {/* Registered users */}\n      <Route path='' element={<PrivateRoute />}>\n        <Route path='/shipping' element={<ShippingScreen />} />\n      </Route>\n      {/* Admin users */}\n      <Route path='' element={<AdminRoute />}>\n        <Route path='/admin/orderlist' element={<OrderListScreen />} />\n      </Route>\n    </Route>\n  )\n);\n\nexport default function Root() {\n  return <RouterProvider router={router} />;\n}\n",
    ),
    (
        "frontend/src/App.js",
        "import { Outlet } from 'react-router-dom';\nimport Header from './components/Header';\nexport default function App() {\n  return (\n    <>\n      <Header />\n      <main><Outlet /></main>\n    </>\n  );\n}\n",
    ),
    (
        "frontend/src/components/Header.js",
        "import { Link } from 'react-router-dom';\nexport default function Header() {\n  return <header><Link to='/cart'>Cart</Link></header>;\n}\n",
    ),
    (
        "frontend/src/components/PrivateRoute.js",
        "import { Navigate, Outlet } from 'react-router-dom';\nexport default function PrivateRoute({ userInfo }) {\n  return userInfo ? <Outlet /> : <Navigate to='/login' replace />;\n}\n",
    ),
    (
        "frontend/src/components/AdminRoute.js",
        "import { Navigate, Outlet } from 'react-router-dom';\nexport default function AdminRoute({ userInfo }) {\n  return userInfo && userInfo.isAdmin ? <Outlet /> : <Navigate to='/login' replace />;\n}\n",
    ),
    (
        "frontend/src/screens/HomeScreen.js",
        "export default function HomeScreen() {\n  return <div>HomeScreen</div>;\n}\n",
    ),
    (
        "frontend/src/screens/CartScreen.js",
        "export default function CartScreen() {\n  return <div>CartScreen</div>;\n}\n",
    ),
    (
        "frontend/src/screens/LoginScreen.js",
        "export default function LoginScreen() {\n  return <div>LoginScreen</div>;\n}\n",
    ),
    (
        "frontend/src/screens/ShippingScreen.js",
        "export default function ShippingScreen() {\n  return <div>ShippingScreen</div>;\n}\n",
    ),
    (
        "frontend/src/screens/admin/OrderListScreen.js",
        "export default function OrderListScreen() {\n  return <div>OrderListScreen</div>;\n}\n",
    ),
];
/// Upstream `nextjs.test.e8ddc4e2.ts:107-198`, file for file.
pub const NEXT_APP: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"site\",\"dependencies\":{\"next\":\"15\",\"react\":\"19\",\"@prisma/client\":\"5\"}}",
    ),
    (
        "lib/db.ts",
        "import { PrismaClient } from '@prisma/client'\nexport const prisma = new PrismaClient()\n",
    ),
    (
        "app/layout.tsx",
        "export default function RootLayout({ children }) {\n  return children\n}\n",
    ),
    (
        "app/page.tsx",
        "import Link from 'next/link'\nexport default function Home() {\n  return (\n    <main>\n      <Link href=\"/users\">Users</Link>\n      <a href=\"/login\">Log in</a>\n      <a href=\"https://example.com\">Elsewhere</a>\n    </main>\n  )\n}\n",
    ),
    (
        "app/login/page.tsx",
        "export default function LoginPage() {\n  return null\n}\n",
    ),
    (
        "app/users/page.tsx",
        "import { NewUserForm } from '../../components/new-user-form'\nimport { prisma } from '../../lib/db'\nexport default async function UsersPage() {\n  const users = await prisma.user.findMany()\n  return <NewUserForm count={users.length} />\n}\n",
    ),
    (
        "app/users/[id]/page.tsx",
        "export default function UserPage({ params }) {\n  return <a href=\"/users\">Back</a>\n}\n",
    ),
    (
        "components/new-user-form.tsx",
        "'use client'\nimport { useCallback, useState } from 'react'\nimport { useRouter } from 'next/navigation'\nimport { createUserAction } from '../app/actions'\nexport function NewUserForm({ count }) {\n  const [email, setEmail] = useState('')\n  const router = useRouter()\n  const handleSubmit = useCallback(async (e) => {\n    e.preventDefault()\n    const user = await createUserAction({ email })\n    if (user.ok) router.push(`/users/${user.id}`)\n  }, [email])\n  return <form onSubmit={handleSubmit}><input value={email} onChange={(e) => setEmail(e.target.value)} /></form>\n}\n",
    ),
    (
        "app/actions.ts",
        "'use server'\nimport { redirect } from 'next/navigation'\nimport { prisma } from '../lib/db'\nexport async function createUserAction(data) {\n  const user = await prisma.user.create({ data })\n  if (!user.verified) redirect('/users')\n  return { ok: true, id: user.id }\n}\n",
    ),
    (
        "app/api/users/route.ts",
        "import { NextResponse } from 'next/server'\nimport { prisma } from '../../../lib/db'\nexport async function GET() {\n  return NextResponse.json(await prisma.user.findMany())\n}\nexport async function POST(req) {\n  const data = await req.json()\n  const user = await prisma.user.create({ data })\n  return NextResponse.json(user, { status: 201 })\n}\n",
    ),
    (
        "middleware.ts",
        "import { NextResponse } from 'next/server'\nexport function middleware(req) {\n  if (!req.cookies.get('session')) {\n    return NextResponse.redirect(new URL('/login', req.url))\n  }\n  return NextResponse.next()\n}\n",
    ),
];
/// Upstream `sveltekit-router.test.e8ddc4e2.ts:57-136`, file for file.
pub const SVELTEKIT_APP: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"conduit\",\"devDependencies\":{\"@sveltejs/kit\":\"2\",\"svelte\":\"5\"}}",
    ),
    (
        "src/routes/+layout.svelte",
        "<script>\n  export let data\n</script>\n<nav>\n  <a href=\"/\">Home</a>\n  <a href=\"/login\">Sign in</a>\n  <a href=\"/settings\">Settings</a>\n  <a href=\"https://example.com\">Elsewhere</a>\n</nav>\n<slot />\n",
    ),
    (
        "src/routes/+page.svelte",
        "<script>\n  export let data\n</script>\n<h1>Conduit</h1>\n<a href=\"/register\">Sign up</a>\n",
    ),
    (
        "src/routes/login/+page.svelte",
        "<script>\n  export let form\n</script>\n<a href=\"/register\">Need an account?</a>\n",
    ),
    (
        "src/routes/login/+page.server.js",
        "import { redirect } from '@sveltejs/kit'\nexport function load({ locals }) {\n  if (locals.user) redirect(307, '/')\n}\nexport const actions = {\n  default: async ({ request, locals }) => {\n    const user = await signIn(request)\n    if (!user) return { errors: ['bad login'] }\n    redirect(307, '/')\n  }\n}\n",
    ),
    (
        "src/routes/register/+page.svelte",
        "<script>\n  export let form\n</script>\n<a href=\"/login\">Have an account?</a>\n",
    ),
    (
        "src/routes/settings/+page.svelte",
        "<script>\n  export let data\n</script>\n<h1>Settings</h1>\n",
    ),
    (
        "src/routes/settings/+page.server.js",
        "import { redirect } from '@sveltejs/kit'\nexport function load({ locals }) {\n  if (!locals.user) redirect(302, '/login')\n}\n",
    ),
    (
        "src/routes/editor/+page.svelte",
        "<script>\n  import { goto } from '$app/navigation'\n  async function publish() {\n    const article = await save()\n    goto(`/article/${article.slug}`)\n  }\n</script>\n<button on:click={publish}>Publish</button>\n",
    ),
    (
        "src/routes/article/[slug]/+page.svelte",
        "<script>\n  export let data\n</script>\n<a href=\"/editor\">Edit</a>\n<a href=\"/profile/@{data.author}\">Author</a>\n",
    ),
    (
        "src/routes/profile/@[user]/+page.svelte",
        "<script>\n  export let data\n</script>\n<h1>Profile</h1>\n",
    ),
    (
        "src/routes/profile/@[user]/+layout.svelte",
        "<script>\n  export let data\n</script>\n<slot />\n",
    ),
    (
        "src/routes/nowhere/+page.server.js",
        "import { redirect } from '@sveltejs/kit'\nexport function load({ url }) {\n  redirect(307, '/no-such-page')\n  redirect(307, url.searchParams.get(\"next\"))\n}\n",
    ),
];
/// Upstream `tanstack-router.test.e8ddc4e2.ts:180-278`, file for file.
pub const TANSTACK_APP: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"app\",\"dependencies\":{\"react\":\"19\",\"@tanstack/react-router\":\"1\"}}",
    ),
    (
        "src/routes/index.tsx",
        "import { createFileRoute, Link } from '@tanstack/react-router'\nexport const Route = createFileRoute('/')({ component: IndexComponent })\nfunction IndexComponent() {\n  return (\n    <div>\n      <Link\n        to=\"/posts/$postId\"\n        params={{ postId: 3 }}\n      >\n        A post\n      </Link>\n      <Link to=\"/login\">Sign in</Link>\n    </div>\n  )\n}\n",
    ),
    (
        "src/routes/posts.route.tsx",
        "import { createFileRoute, Outlet } from '@tanstack/react-router'\nexport const Route = createFileRoute('/posts')({ component: PostsLayout })\nfunction PostsLayout() {\n  return <Outlet />\n}\n",
    ),
    (
        "src/routes/posts.index.tsx",
        "import { createFileRoute } from '@tanstack/react-router'\nexport const Route = createFileRoute('/posts/')({ component: PostsIndexComponent })\nfunction PostsIndexComponent() {\n  return <div>Posts</div>\n}\n",
    ),
    (
        "src/routes/posts.$postId.tsx",
        "import { createFileRoute } from '@tanstack/react-router'\nexport const Route = createFileRoute('/posts/$postId')({ component: PostComponent })\nfunction PostComponent() {\n  return <div>Post</div>\n}\n",
    ),
    (
        "src/routes/login.tsx",
        "import { createFileRoute, useNavigate } from '@tanstack/react-router'\nexport const Route = createFileRoute('/login')({ component: LoginComponent })\nfunction LoginComponent() {\n  const navigate = useNavigate()\n  async function submit(creds) {\n    const ok = await signIn(creds)\n    if (ok) navigate({ to: '/dashboard' })\n  }\n  return <form onSubmit={submit} />\n}\n",
    ),
    (
        "src/routes/_auth.tsx",
        "import { createFileRoute, redirect } from '@tanstack/react-router'\nexport const Route = createFileRoute('/_auth')({\n  beforeLoad: ({ context }) => {\n    if (context.auth.status === 'loggedOut') {\n      throw redirect({ to: '/login' })\n    }\n  },\n})\n",
    ),
    (
        "src/routes/_auth.dashboard.tsx",
        "import { createFileRoute, Link } from '@tanstack/react-router'\nexport const Route = createFileRoute('/_auth/dashboard')({ component: DashboardComponent })\nfunction DashboardComponent() {\n  return <Link to=\"/posts\">All posts</Link>\n}\n",
    ),
    (
        "src/routes/settings.tsx",
        "import { createFileRoute, useNavigate } from '@tanstack/react-router'\nexport const Route = createFileRoute('/settings')({ component: SettingsComponent })\nfunction SettingsComponent() {\n  const navigate = useNavigate()\n  function nowhere() {\n    navigate({ to: '/no-such-route' })\n  }\n  function filter() {\n    navigate({ search: (old) => ({ ...old, page: 2 }) })\n  }\n  return <button onClick={nowhere} />\n}\n",
    ),
];
/// Upstream `vue-router.test.e8ddc4e2.ts:118-217`, file for file.
pub const VUE_APP: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"conduit\",\"dependencies\":{\"vue\":\"3\",\"vue-router\":\"4\"}}",
    ),
    (
        "src/router/index.js",
        "import { createRouter, createWebHistory } from \"vue-router\"\nconst router = createRouter({\n  history: createWebHistory(),\n  routes: [\n    { name: \"home\", path: \"/\", component: () => import(\"@/views/Home\") },\n    { name: \"login\", path: \"/login\", component: () => import(\"@/views/Login\") },\n    { name: \"register\", path: \"/register\", component: () => import(\"@/views/Register\") },\n    { name: \"settings\", path: \"/settings\", component: () => import(\"@/views/Settings\") },\n    { name: \"profile\", path: \"/profile/:username\", component: () => import(\"@/views/Profile\") }\n  ]\n})\nexport default router\n",
    ),
    (
        "src/views/Home.vue",
        "<template>\n  <div><TheHeader /></div>\n</template>\n<script setup>\nimport { useRouter } from \"vue-router\"\nimport TheHeader from \"@/components/TheHeader.vue\"\nconst router = useRouter()\nfunction goTo(tag) {\n  router.push({ path: \"/\", query: { tag } })\n}\n</script>\n",
    ),
    (
        "src/views/Login.vue",
        "<template>\n  <form @submit=\"submit\"><router-link :to=\"{ name: 'register' }\">Need an account?</router-link></form>\n</template>\n<script setup>\nimport { useRouter } from \"vue-router\"\nconst router = useRouter()\nfunction submit() {\n  login().then(() => router.push({ name: \"home\" }))\n}\n</script>\n",
    ),
    (
        "src/views/Register.vue",
        "<template>\n  <router-link to=\"/login\">Have an account?</router-link>\n</template>\n<script setup>\nconst nothing = 1\n</script>\n",
    ),
    (
        "src/views/Settings.vue",
        "<template>\n  <button @click=\"save\">Save</button>\n</template>\n<script setup>\nimport { useRouter } from \"vue-router\"\nconst router = useRouter()\nconst target = \"/nowhere\"\nfunction save(user) {\n  router.push({ name: \"profile\", params: { username: user.username } })\n}\nfunction bail() {\n  router.push(target)\n}\n</script>\n",
    ),
    (
        "src/views/Profile.vue",
        "<template>\n  <div>Profile</div>\n</template>\n<script setup>\nconst x = 1\n</script>\n",
    ),
    (
        "src/components/TheHeader.vue",
        "<template>\n  <nav>\n    <router-link :to=\"{ name: 'home' }\">Home</router-link>\n    <router-link to=\"/settings\">Settings</router-link>\n    <a href=\"https://example.com\">Elsewhere</a>\n  </nav>\n</template>\n<script setup>\nconst y = 1\n</script>\n",
    ),
    (
        "src/utils/trail.js",
        "export function trail() {\n  const paths = []\n  paths.push(\"/login\")\n  return paths\n}\n",
    ),
];

/// Upstream `angular-router.test.e8ddc4e2.ts:201-212`: a component class as
/// the fixture writes one, its template inline or in a file of its own.
fn ng_component(name: &str, selector: &str, template: &str, body: &str) -> String {
    let template = if template.starts_with("./") {
        format!("templateUrl: '{template}'")
    } else {
        format!("template: `{template}`")
    };
    format!(
        "import {{ Component }} from '@angular/core';\nimport {{ Router }} from '@angular/router';\n@Component({{\n  selector: '{selector}',\n  {template},\n}})\nexport class {name} {{\n  constructor(private readonly router: Router) {{}}\n{body}\n}}\n"
    )
}

const NG_ROUTER_IMPORT: &str = "import { Router } from '@angular/router';";
const NG_ROUTER_AND_ROUTES: &str = "import { Router } from '@angular/router';\nimport { internalRoutes } from '../routes.constants';";

/// Upstream `angular-router.test.e8ddc4e2.ts:214-302`, "an Angular app,
/// indexed", file for file.
pub fn angular_app() -> Vec<(String, String)> {
    let file = |path: &str, text: &str| (path.to_string(), text.to_string());
    let home = ng_component(
        "HomeComponent",
        "app-home",
        "./home.component.html",
        "  open(slug: string) { this.router.navigate(['/login']); }\n  more() { this.router.navigate([], { queryParams: { page: 2 } }); }",
    )
    .replacen("export class", "export default class", 1)
    .replacen(NG_ROUTER_IMPORT, NG_ROUTER_AND_ROUTES, 1)
    .replacen(
        "  constructor(",
        "  protected readonly routerLinkAbout = internalRoutes.about.routerLink;\n  constructor(",
        1,
    );
    vec![
        file(
            "package.json",
            "{\"dependencies\":{\"@angular/core\":\"^19.0.0\",\"@angular/router\":\"^19.0.0\"}}",
        ),
        file(
            "src/app/routes.constants.ts",
            "export const internalRoutes = {\n  account: { path: 'account', routerLink: ['/account'], subRoutes: { access: { path: 'access', routerLink: (id: string) => ['/account', 'access', id] } } },\n  about: { path: $localize`:kebab-case@@routes.about:about`, routerLink: ['/' + $localize`:kebab-case@@routes.about:about`] },\n};\n",
        ),
        file(
            "src/app/app.routes.ts",
            "import { Routes } from '@angular/router';\nimport { AuthComponent } from './auth/auth.component';\nimport { internalRoutes } from './routes.constants';\nconst { about } = internalRoutes;\nexport const routes: Routes = [\n  { path: 'home', loadComponent: () => import('./home/home.component') },\n  { path: 'login', component: AuthComponent },\n  { path: about.path, loadComponent: () => import('./about/about.component').then((m) => m.AboutComponent) },\n  { path: internalRoutes.account.path, loadChildren: () => import('./account/account.routes') },\n  { path: 'admin', loadChildren: () => import('./admin/admin.module').then((m) => m.AdminModule) },\n  { path: '**', redirectTo: 'home' },\n];\n",
        ),
        file(
            "src/app/account/account.routes.ts",
            "import { Routes } from '@angular/router';\nimport { AccountComponent } from './account.component';\nimport { AccessComponent } from './access.component';\nimport { internalRoutes } from '../routes.constants';\nexport default [\n  { path: '', component: AccountComponent, children: [\n      { path: '', loadComponent: () => import('./overview.component') },\n      { path: internalRoutes.account.subRoutes.access.path + '/:id', component: AccessComponent },\n  ] },\n] satisfies Routes;\n",
        ),
        file(
            "src/app/admin/admin.module.ts",
            "import { NgModule } from '@angular/core';\nimport { AdminRoutingModule } from './admin-routing.module';\n@NgModule({ imports: [AdminRoutingModule] })\nexport class AdminModule {}\n",
        ),
        file(
            "src/app/admin/admin-routing.module.ts",
            "import { NgModule } from '@angular/core';\nimport { RouterModule, Routes } from '@angular/router';\nimport { AdminUsersComponent } from './admin-users.component';\nconst routes: Routes = [{ path: '', redirectTo: 'users', pathMatch: 'full' }, { path: 'users', component: AdminUsersComponent }];\n@NgModule({ imports: [RouterModule.forChild(routes)], exports: [RouterModule] })\nexport class AdminRoutingModule {}\n",
        ),
        ("src/app/home/home.component.ts".to_string(), home),
        file(
            "src/app/home/home.component.html",
            "<app-card></app-card>\n<a routerLink=\"/login\">Sign in</a>\n<a [routerLink]=\"routerLinkAbout\">About</a>\n<a [routerLink]=\"['/account', 'access', 7]\">Access</a>\n<a routerLink=\"relative\">Relative</a>\n",
        ),
        (
            "src/app/shared/card.component.ts".to_string(),
            ng_component(
                "CardComponent",
                "app-card",
                "<button (click)=\"save()\">Save</button><input [(ngModel)]=\"name\" /><a (click)=\"closed.emit(true)\">x</a>",
                "  name = '';\n  save() { this.router.navigate(internalRoutes.account.subRoutes.access.routerLink('me')); }",
            )
            .replacen(NG_ROUTER_IMPORT, NG_ROUTER_AND_ROUTES, 1),
        ),
        (
            "src/app/auth/auth.component.ts".to_string(),
            ng_component(
                "AuthComponent",
                "app-auth",
                "<a routerLink=\"/home\">Home</a>",
                "  done() { this.router.navigate(['/']); }",
            ),
        ),
        (
            "src/app/about/about.component.ts".to_string(),
            ng_component("AboutComponent", "app-about", "<p>About</p>", ""),
        ),
        (
            "src/app/account/account.component.ts".to_string(),
            ng_component(
                "AccountComponent",
                "app-account",
                "<a [routerLink]=\"['/login']\">Log out</a><app-tabs [tabs]=\"tabs\"></app-tabs><router-outlet></router-outlet>",
                "  tabs = [{ label: 'Access', routerLink: internalRoutes.account.subRoutes.access.routerLink('me') }];",
            )
            .replacen(NG_ROUTER_IMPORT, NG_ROUTER_AND_ROUTES, 1),
        ),
        (
            "src/app/account/overview.component.ts".to_string(),
            ng_component("OverviewComponent", "app-overview", "<p>Overview</p>", "")
                .replacen("export class", "export default class", 1),
        ),
        (
            "src/app/account/access.component.ts".to_string(),
            ng_component("AccessComponent", "app-access", "<p>Access</p>", ""),
        ),
        (
            "src/app/admin/admin-users.component.ts".to_string(),
            ng_component("AdminUsersComponent", "app-admin-users", "<p>Users</p>", ""),
        ),
    ]
}

/// Upstream `ui-steps-api.test.e8ddc4e2.ts:30-158`, file for file.
pub const STEPS_MOBILE: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"app\",\"dependencies\":{\"expo\":\"52\",\"expo-router\":\"4\",\"react-native\":\"0.76\"}}",
    ),
    (
        "src/app/_layout.tsx",
        "export default function Layout() { return null }\n",
    ),
    (
        "src/app/index.tsx",
        "import { router } from 'expo-router'\nexport default function Home() {\n  return null\n}\n",
    ),
    (
        "src/components/capture/capture-view.tsx",
        "import { NativeModules, NativeEventEmitter } from 'react-native'\nexport const captureView = NativeModules.CaptureView\nexport const nativeEmitter = new NativeEventEmitter(NativeModules.CaptureEvents)\n",
    ),
    (
        "src/api/client.ts",
        "import axios from 'axios'\nexport const client = axios.create({ baseURL: 'x' })\n",
    ),
    (
        "src/api/frames.ts",
        "import { client } from './client'\nexport async function uploadARCapture(uri: string) {\n  await client.post('/frames', { uri })\n  return client.get('/frames/status')\n}\n",
    ),
    (
        "src/storage/capture.storage.ts",
        "import { create } from 'zustand'\nconst useCaptureStorage = create<State>((set) => ({\n  zipUri: null,\n  setZipUri: (zipUri: string) => set({ zipUri }),\n}))\nexport default useCaptureStorage\n",
    ),
    (
        "src/app/capture/review.tsx",
        "import { useCallback, useEffect } from 'react'\nimport { router } from 'expo-router'\nimport { captureView, nativeEmitter } from '../../components/capture/capture-view'\nimport { uploadARCapture } from '../../api/frames'\nimport useCaptureStorage from '../../storage/capture.storage'\nexport default function ReviewScreen({ unlimited }: { unlimited: boolean }) {\n  const setZipUri = useCaptureStorage((s) => s.setZipUri)\n  const handleApprove = useCallback(() => {\n    captureView.finalizeCaptureSession()\n  }, [])\n  const handleZipComplete = useCallback(async (data: { uri: string }) => {\n    setZipUri(data.uri)\n    await uploadARCapture(data.uri)\n    Alert.alert('Uploaded', data.uri, [{ text: 'OK' }])\n    if (unlimited) router.replace('/')\n  }, [unlimited])\n  useEffect(() => {\n    const sub = nativeEmitter.addListener('onZipComplete', handleZipComplete)\n    return () => sub.remove()\n  }, [handleZipComplete])\n  const form = useForm({ onSubmit: () => handleSubmit() })\n  function handleSubmit() {\n    captureView.finalizeCaptureSession()\n  }\n  return <Button onPress={handleApprove} />\n}\n",
    ),
    (
        "src/app/capture/index.tsx",
        "import { memo, useCallback } from 'react'\nimport { captureView } from '../../components/capture/capture-view'\nfunction CaptureComponent() {\n  const handleOpen = useCallback(() => {\n    captureView.finalizeCaptureSession()\n  }, [])\n  return <Button onPress={() => handleOpen()} />\n}\nconst MemoizedCaptureComponent = memo(CaptureComponent)\nexport default function CapturePage() {\n  return <MemoizedCaptureComponent />\n}\n",
    ),
    (
        "ios/CaptureView.m",
        "#import <React/RCTViewManager.h>\n@interface RCT_EXTERN_MODULE(CaptureView, RCTViewManager)\nRCT_EXTERN_METHOD(finalizeCaptureSession)\n@end\n",
    ),
    (
        "ios/CaptureView.swift",
        "import Foundation\nclass CaptureView: RCTViewManager {\n  @objc func finalizeCaptureSession() {\n    let result = zip()\n    if result {\n      CaptureEvents.shared.emitZipComplete()\n    }\n  }\n  func zip() -> Bool { return true }\n}\n",
    ),
    (
        "ios/CaptureEvents.swift",
        "import Foundation\nclass CaptureEvents: RCTEventEmitter {\n  static let shared = CaptureEvents()\n  func emitZipComplete() {\n    sendEvent(withName: \"onZipComplete\", body: nil)\n  }\n}\n",
    ),
    (
        "src/api/remove-thing.ts",
        "import { client } from './client'\nexport async function removeThing(name: string) {\n  await client.post('/things/remove', { name })\n}\n",
    ),
    (
        "src/app/confirm.tsx",
        "import { Alert, Button } from 'react-native'\nimport { removeThing } from '../api/remove-thing'\nexport default function ConfirmScreen() {\n  return (\n    <Button\n      title=\"remove\"\n      onPress={() =>\n        Alert.prompt('Remove thing', 'Which one?', [\n          { text: 'Cancel' },\n          { text: 'OK', onPress: (name) => { if (name) removeThing(name) } },\n        ])\n      }\n    />\n  )\n}\n",
    ),
];
/// Upstream `ui-steps-api-servers.test.e8ddc4e2.ts:28-265`, file for file.
pub const STEPS_SERVERS: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"api\",\"dependencies\":{\"express\":\"4\",\"@nestjs/core\":\"10\",\"@nestjs/common\":\"10\",\"bullmq\":\"5\",\"@prisma/client\":\"5\",\"typeorm\":\"0.3\"}}",
    ),
    (
        "src/server/db.ts",
        "import { PrismaClient } from '@prisma/client'\nexport const prisma = new PrismaClient()\n",
    ),
    (
        "src/server/queue.ts",
        "import { Queue } from 'bullmq'\nexport const emailQueue = new Queue('email')\n",
    ),
    (
        "src/server/errors.ts",
        "export class NotFoundError extends Error {}\n",
    ),
    (
        "src/server/auth.ts",
        "export function authenticate(req, res, next) {\n  next()\n}\n",
    ),
    (
        "src/server/validate.ts",
        "export function validate(schema) {\n  return (req, res, next) => next()\n}\n",
    ),
    (
        "src/server/users.service.ts",
        "import { prisma } from './db'\nimport { emailQueue } from './queue'\nimport { NotFoundError } from './errors'\nexport async function createUser(req, res) {\n  const user = await prisma.user.create({ data: { email: req.body.email, name: req.body.name } })\n  await emailQueue.add('welcome', { userId: user.id })\n  if (!user.verified) {\n    await sendVerification(user)\n  }\n  res.status(201).json({\n    id: user.id,\n    token: signToken(user.id),\n  })\n}\nfunction signToken(id) {\n  return jwt.sign({ id }, process.env.JWT_SECRET, { expiresIn: '1d' })\n}\nexport async function getUser(id: string) {\n  const user = await prisma.user.findUnique({ where: { id } })\n  if (!user) throw new NotFoundError('no such user')\n  return user\n}\nasync function sendVerification(user) {\n  await transporter.sendMail({ to: user.email })\n}\nexport async function acceptUser(req, res) {\n  const user = await prisma.user.update({ where: { id: req.params.id }, data: { accepted: true } })\n  res.status(202)\n  res.json(user)\n}\n",
    ),
    (
        "src/server/users.routes.ts",
        "import { Router } from 'express'\nimport { authenticate } from './auth'\nimport { validate } from './validate'\nimport { createUser, getUser, acceptUser } from './users.service'\nconst router = Router()\nrouter.post('/users', authenticate, validate(userSchema), createUser)\nrouter.post('/users/:id/accept', authenticate, acceptUser)\nrouter.get('/users/:id', authenticate, async (req, res) => {\n  const user = await getUser(req.params.id)\n  res.json(user)\n})\nexport default router\n",
    ),
    (
        "src/nest/cats.service.ts",
        "import { Injectable } from '@nestjs/common'\nimport { InjectRepository } from '@nestjs/typeorm'\nimport { Repository } from 'typeorm'\nimport { InjectQueue } from '@nestjs/bullmq'\nimport { Queue } from 'bullmq'\nimport { Cat } from './cat.entity'\n@Injectable()\nexport class CatsService {\n  constructor(\n    @InjectRepository(Cat) private readonly catsRepository: Repository<Cat>,\n    @InjectQueue('cats') private readonly catsQueue: Queue\n  ) {}\n  async create(dto) {\n    const cat = await this.catsRepository.save(dto)\n    await this.catsQueue.add('index', { id: cat.id })\n    return cat\n  }\n  async findOne(id: string) {\n    return this.catsRepository.findOne({ where: { id } })\n  }\n}\n",
    ),
    (
        "src/nest/cat.entity.ts",
        "import { Entity } from 'typeorm'\n@Entity()\nexport class Cat {\n  id: string\n}\n",
    ),
    (
        "src/nest/cats.controller.ts",
        "import { Controller, Get, Post, Body, Param, UseGuards, NotFoundException } from '@nestjs/common'\nimport { AuthGuard } from '@nestjs/passport'\nimport { CatsService } from './cats.service'\nimport { RolesGuard } from './roles.guard'\n@Controller('cats')\n@UseGuards(AuthGuard('jwt'))\nexport class CatsController {\n  constructor(private readonly catsService: CatsService) {}\n  @Post()\n  @UseGuards(RolesGuard)\n  async create(@Body() dto: CreateCatDto) {\n    return this.catsService.create(dto)\n  }\n  @Get(':id')\n  async findOne(@Param('id') id: string) {\n    const cat = await this.catsService.findOne(id)\n    if (!cat) throw new NotFoundException('no cat')\n    return cat\n  }\n}\n",
    ),
    (
        "src/nest/roles.guard.ts",
        "import { Injectable } from '@nestjs/common'\n@Injectable()\nexport class RolesGuard {\n  canActivate() { return true }\n}\n",
    ),
    (
        "src/nest/cats.processor.ts",
        "import { Processor, Process } from '@nestjs/bull'\n@Processor('cats')\nexport class CatsProcessor {\n  @Process('index')\n  async handleIndex(job) {\n    await searchClient.index(job.data)\n  }\n}\n",
    ),
    (
        "api/items.py",
        "from fastapi import APIRouter, Depends, HTTPException\nfrom sqlmodel import select\nfrom .deps import get_current_user, SessionDep\nfrom .models import Item, ItemCreate\nfrom .tasks import send_welcome\n\nrouter = APIRouter()\n\n@router.post(\"/items\", dependencies=[Depends(get_current_user)])\ndef create_item(session: SessionDep, item_in: ItemCreate):\n    item = Item.model_validate(item_in)\n    session.add(item)\n    session.commit()\n    if item.price < 0:\n        raise HTTPException(status_code=422, detail=\"bad price\")\n    send_welcome.delay(item.id)\n    return item\n",
    ),
    (
        "api/deps.py",
        "def get_current_user():\n    return None\n\nSessionDep = None\n",
    ),
    (
        "api/models.py",
        "class Item:\n    pass\n\nclass ItemCreate:\n    pass\n",
    ),
    (
        "api/tasks.py",
        "from celery import shared_task\n\n@shared_task\ndef send_welcome(item_id):\n    return item_id\n",
    ),
    (
        "api/orders.py",
        "from fastapi import APIRouter\n\nrouter = APIRouter(prefix=\"/orders\", tags=[\"orders\"])\n\n@router.get(\"/\")\ndef list_orders():\n    return []\n\n@router.get(\"/{order_id}\")\ndef get_order(order_id: int):\n    return order_id\n",
    ),
    (
        "api/v1.py",
        "from fastapi import APIRouter\nfrom .orders import router as orders_router\napi_router = APIRouter()\napi_router.include_router(orders_router)\n",
    ),
    (
        "api/main.py",
        "from fastapi import FastAPI\nfrom .items import router\nfrom .v1 import api_router\nfrom .config import settings\napp = FastAPI()\napp.include_router(router)\napp.include_router(api_router, prefix=\"/api/v1\")\napp.include_router(api_router, prefix=settings.LEGACY)\n",
    ),
    ("api/config.py", "settings = None\n"),
    ("requirements.txt", "fastapi\nsqlmodel\ncelery\n"),
    (
        "src/main/java/demo/OwnerController.java",
        "package demo;\nimport org.springframework.web.bind.annotation.*;\nimport org.springframework.http.*;\n@RestController\n@RequestMapping(\"/owners\")\npublic class OwnerController {\n  private final OwnerRepository owners;\n  public OwnerController(OwnerRepository owners) { this.owners = owners; }\n  @PostMapping(\"/new\")\n  @PreAuthorize(\"hasRole('ADMIN')\")\n  public ResponseEntity<Owner> create(@RequestBody Owner owner) {\n    if (owner.getName() == null) {\n      return ResponseEntity.badRequest().build();\n    }\n    Owner saved = owners.save(owner);\n    return ResponseEntity.status(HttpStatus.CREATED).body(saved);\n  }\n}\n",
    ),
    (
        "src/main/java/demo/OwnerRepository.java",
        "package demo;\nimport org.springframework.data.jpa.repository.JpaRepository;\npublic interface OwnerRepository extends JpaRepository<Owner, Integer> {\n}\n",
    ),
    (
        "src/main/java/demo/Owner.java",
        "package demo;\npublic class Owner {\n  private String name;\n  public String getName() { return name; }\n}\n",
    ),
    (
        "src/Web/Endpoints/TodoItems.cs",
        "using Microsoft.AspNetCore.Http.HttpResults;\nnamespace Demo.Web.Endpoints;\npublic class TodoItems : IEndpointGroup\n{\n    public static void Map(RouteGroupBuilder groupBuilder)\n    {\n        groupBuilder.RequireAuthorization();\n        groupBuilder.MapPost(CreateTodoItem);\n        groupBuilder.MapPut(UpdateTodoItem, \"{id}\");\n    }\n    public static async Task<Created<int>> CreateTodoItem(ISender sender, CreateTodoItemCommand command)\n    {\n        var id = await sender.Send(command);\n        return TypedResults.Created($\"/{nameof(TodoItems)}/{id}\", id);\n    }\n    public static async Task<Results<NoContent, BadRequest>> UpdateTodoItem(ISender sender, int id, UpdateTodoItemCommand command)\n    {\n        if (id != command.Id)\n            return TypedResults.BadRequest();\n        await sender.Send(command);\n        return TypedResults.NoContent();\n    }\n}\n",
    ),
    (
        "src/Web/Infrastructure/WebApplicationExtensions.cs",
        "using Microsoft.AspNetCore.Builder;\nnamespace Demo.Web.Infrastructure;\npublic static class WebApplicationExtensions\n{\n    public static WebApplication MapEndpoints(this WebApplication app)\n    {\n        var groupName = \"x\";\n        var group = app.MapGroup($\"/api/{groupName}\").WithTags(groupName);\n        return app;\n    }\n}\n",
    ),
];
/// Upstream `ui-steps-cross-tier.test.e8ddc4e2.ts:30-278`, file for file.
pub const CROSS_TIER: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"name\":\"mono\",\"workspaces\":[\"apps/*\"],\"dependencies\":{\"next\":\"15\",\"react\":\"19\",\"express\":\"4\",\"axios\":\"1\",\"bullmq\":\"5\",\"@nestjs/common\":\"10\",\"@nestjs/core\":\"10\",\"@nestjs/bull\":\"10\",\"@nestjs/event-emitter\":\"2\",\"@nestjs/websockets\":\"10\",\"socket.io\":\"4\",\"socket.io-client\":\"4\",\"@prisma/client\":\"5\"}}",
    ),
    (
        "apps/web/app/users/page.tsx",
        "import { NewUserForm } from '../../components/new-user-form'\nexport default function UsersPage() {\n  return <NewUserForm />\n}\n",
    ),
    (
        "apps/web/components/new-user-form.tsx",
        "'use client'\nimport { useCallback, useState } from 'react'\nimport { createUserAction } from '../app/actions'\nexport function NewUserForm() {\n  const [email, setEmail] = useState('')\n  const handleSubmit = useCallback(async (e) => {\n    e.preventDefault()\n    if (!email) return\n    const res = await fetch('/api/users', { method: 'POST', body: JSON.stringify({ email }) })\n    if (res.ok) await createUserAction({ email })\n  }, [email])\n  return <form onSubmit={handleSubmit}><input value={email} onChange={(e) => setEmail(e.target.value)} /></form>\n}\n",
    ),
    (
        "apps/web/app/actions.ts",
        "'use server'\nimport { prisma } from '../lib/db'\nimport { redirect } from 'next/navigation'\nexport async function createUserAction(data) {\n  await prisma.user.create({ data })\n  redirect('/users')\n}\n",
    ),
    (
        "apps/web/lib/db.ts",
        "import { PrismaClient } from '@prisma/client'\nexport const prisma = new PrismaClient()\n",
    ),
    (
        "apps/web/lib/api.ts",
        "import axios from 'axios'\nexport const api = axios.create({ baseURL: '/api' })\n",
    ),
    (
        "apps/web/components/user-card.tsx",
        "'use client'\nimport { api } from '../lib/api'\nexport function UserCard({ id, url }) {\n  async function load() {\n    const { data } = await api.get(`/users/${id}`)\n    const external = await fetch('https://api.stripe.com/v1/charges')\n    const dynamic = await fetch(url)\n    const orders = await fetch(`${process.env.API_URL}/api/users/${id}/orders`)\n    return [data, external, dynamic, orders]\n  }\n  return null\n}\n",
    ),
    (
        "apps/web/components/chat.tsx",
        "'use client'\nimport { useEffect, useState } from 'react'\nimport { io } from 'socket.io-client'\nconst socket = io()\nexport function Chat() {\n  const [messages, setMessages] = useState([])\n  useEffect(() => {\n    socket.on('message', (m) => {\n      setMessages((prev) => [...prev, m])\n    })\n  }, [])\n  function send(text) {\n    socket.emit('message', text)\n  }\n  return null\n}\n",
    ),
    (
        "apps/api/src/app.ts",
        "import express from 'express'\nimport { createUser, getUser, listOrders } from './users'\nconst app = express()\napp.post('/api/users', createUser)\napp.get('/api/users/:id', getUser)\napp.get('/api/users/:id/orders', listOrders)\nconst v1 = require('./v1')\napp.use('/api/v1', authenticate, v1)\nexport default app\n",
    ),
    (
        "apps/api/src/v1/index.ts",
        "import { Router } from 'express'\nimport ordersRouter from '../orders.routes'\nconst router = Router()\nrouter.use('/orders', ordersRouter)\nexport default router\n",
    ),
    (
        "apps/api/src/orders.routes.ts",
        "import { Router } from 'express'\nimport { prisma } from './db'\nconst router = Router()\nrouter.get('/', listAllOrders)\nrouter.post('/:id/refund', refund)\nexport async function listAllOrders(req, res) {\n  res.json(await prisma.order.findMany())\n}\nexport async function refund(req, res) {\n  res.status(202).end()\n}\nexport default router\n",
    ),
    (
        "apps/web/components/orders.tsx",
        "'use client'\nimport useSWR from 'swr'\nexport function Orders() {\n  const { data } = useSWR<Order[]>('/api/v1/orders', fetcher)\n  async function loadOrders() {\n    const res = await fetch('/api/v1/orders')\n    return res.json()\n  }\n  return data\n}\n",
    ),
    (
        "apps/api/src/users.ts",
        "import { prisma } from './db'\nimport { emailQueue, reportQueue } from './queue'\nexport async function createUser(req, res) {\n  const user = await prisma.user.create({ data: req.body })\n  await emailQueue.add('welcome', { userId: user.id })\n  if (req.body.plan) {\n    await reportQueue.add('monthly', { userId: user.id })\n  }\n  res.status(201).json(user)\n}\nexport async function getUser(req, res) {\n  const user = await prisma.user.findUnique({ where: { id: req.params.id } })\n  res.json(user)\n}\nexport async function listOrders(req, res) {\n  res.json(await prisma.order.findMany({ where: { userId: req.params.id } }))\n}\n",
    ),
    (
        "apps/api/src/db.ts",
        "import { PrismaClient } from '@prisma/client'\nexport const prisma = new PrismaClient()\n",
    ),
    (
        "apps/api/src/queue.ts",
        "import { Queue } from 'bullmq'\nexport const emailQueue = new Queue('email')\nexport const reportQueue = new Queue('reports')\n",
    ),
    (
        "apps/api/src/email.processor.ts",
        "import { Processor, Process } from '@nestjs/bull'\n@Processor('email')\nexport class EmailProcessor {\n  constructor(private readonly mailer: MailerService) {}\n  @Process('welcome')\n  async sendWelcome(job) {\n    await this.mailer.sendMail({ to: job.data.email })\n  }\n}\n",
    ),
    (
        "apps/api/src/reports.worker.ts",
        "import { Worker } from 'bullmq'\nexport const reportWorker = new Worker('reports', async (job) => {\n  await buildReport(job.data)\n})\nexport async function buildReport(data) {\n  return data\n}\n",
    ),
    (
        "apps/api/src/users.service.ts",
        "import { Injectable } from '@nestjs/common'\nimport { EventEmitter2 } from '@nestjs/event-emitter'\n@Injectable()\nexport class UsersService {\n  constructor(private readonly eventEmitter: EventEmitter2) {}\n  async create(dto) {\n    const user = { id: 1, ...dto }\n    this.eventEmitter.emit('user.created', user)\n    return user\n  }\n}\n",
    ),
    (
        "apps/api/src/notifications.listener.ts",
        "import { Injectable } from '@nestjs/common'\nimport { OnEvent } from '@nestjs/event-emitter'\n@Injectable()\nexport class NotificationsListener {\n  @OnEvent('user.created')\n  handleUserCreated(user) {\n    return notify(user)\n  }\n  @OnEvent('user.*')\n  audit(payload) {\n    return log(payload)\n  }\n  @OnEvent('order.paid')\n  handleOrderPaid(order) {\n    return order\n  }\n}\n",
    ),
    (
        "apps/api/src/chat.gateway.ts",
        "import { WebSocketGateway, SubscribeMessage, WebSocketServer } from '@nestjs/websockets'\n@WebSocketGateway()\nexport class ChatGateway {\n  @WebSocketServer() server\n  @SubscribeMessage('message')\n  handleMessage(client, payload) {\n    this.server.emit('message', payload)\n    return payload\n  }\n}\n",
    ),
    (
        "apps/api/src/__tests__/users.test.ts",
        "import { it } from 'vitest'\nit('creates a user', async () => {\n  await fetch('/api/users', { method: 'POST' })\n})\n",
    ),
];
/// Upstream `steps-server-rendered-replies.test.e8ddc4e2.ts:91-139`, file for file.
pub const REPLIES_SPRING: &[(&str, &str)] = &[
    (
        "pom.xml",
        "<project><dependencies><dependency><artifactId>spring-boot-starter-web</artifactId></dependency></dependencies></project>",
    ),
    (
        "src/main/java/app/owner/OwnerController.java",
        "package app.owner;\n@Controller\nclass OwnerController {\n    private static final String VIEWS_FORM = \"owners/createOrUpdateOwnerForm\";\n    @PostMapping(\"/owners/new\")\n    public String processCreationForm(Owner owner, BindingResult result) {\n        if (result.hasErrors()) {\n            return VIEWS_FORM;\n        }\n        return \"redirect:/owners/\" + owner.getId();\n    }\n    @GetMapping(\"/owners/{ownerId}\")\n    public ModelAndView showOwner(@PathVariable int ownerId) {\n        ModelAndView mav = new ModelAndView(\"owners/ownerDetails\");\n        return mav;\n    }\n    @GetMapping(\"/owners/legacy\")\n    public RedirectView legacy() {\n        return new RedirectView(\"/owners\");\n    }\n}\n",
    ),
    (
        "src/main/java/app/owner/StatusController.java",
        "package app.owner;\n@RestController\nclass StatusController {\n    @GetMapping(\"/status\")\n    public String status() { return \"ok\"; }\n}\n",
    ),
];
/// Upstream `steps-server-rendered-replies.test.e8ddc4e2.ts:140-178`, file for file.
pub const REPLIES_LARAVEL: &[(&str, &str)] = &[
    ("artisan", "#!/usr/bin/env php\n<?php\n"),
    (
        "composer.json",
        "{\"require\":{\"laravel/framework\":\"^11.0\"}}",
    ),
    (
        "routes/web.php",
        "<?php\nRoute::get('/books/{slug}', [BookController::class, 'show']);\nRoute::post('/books', [BookController::class, 'store']);\n",
    ),
    (
        "app/Http/Controllers/BookController.php",
        "<?php\nclass BookController {\n    public function show(string $slug) {\n        $book = Book::findBySlug($slug);\n        if (is_null($book)) {\n            return redirect('/books');\n        }\n        return view('books.show', ['book' => $book]);\n    }\n    public function store() {\n        return redirect()->back()->withErrors(['name' => 'required']);\n    }\n}\n",
    ),
    (
        "app/Http/Controllers/DocsController.php",
        "<?php\nclass DocsController { public function redirect() { return 1; } }\n",
    ),
    (
        "app/Views/Block.php",
        "<?php\nclass Block { public $view; }\n",
    ),
    (
        "app/Models/View.php",
        "<?php\nclass View { public static function incrementFor($x) {} }\n",
    ),
];
/// Upstream `steps-server-rendered-replies.test.e8ddc4e2.ts:179-201`, file for file.
pub const REPLIES_LARAVEL_BINDS: &[(&str, &str)] = &[
    ("artisan", "#!/usr/bin/env php\n<?php\n"),
    (
        "composer.json",
        "{\"require\":{\"laravel/framework\":\"^11.0\"}}",
    ),
    (
        "app/helpers.php",
        "<?php\nfunction format_title($t) { return ucfirst($t); }\n",
    ),
    (
        "app/Http/Controllers/PageController.php",
        "<?php\nclass PageController {\n    public function show() {\n        $this->setTitle(format_title('x'));\n    }\n    protected function setTitle($t) {}\n}\n",
    ),
];
/// Upstream `swiftui-viewer-noise.test.e8ddc4e2.ts:139-160`, file for file.
pub const SWIFTUI_NOTIFICATIONS: &[(&str, &str)] = &[
    (
        "App/Endpoints.swift",
        "enum Notifications {\n    case notificationsV2(sinceId: String?)\n}\n",
    ),
    (
        "App/NotificationsViewModel.swift",
        "final class NotificationsViewModel {\n    func refresh(client: Client) async {\n        _ = try? await client.get(endpoint: Notifications.notificationsV2(sinceId: nil))\n    }\n}\n",
    ),
];

/// Upstream `angular-router.test.e8ddc4e2.ts:400-503`, file for file.
pub const NG_BINDINGS: &[(&str, &str)] = &[
    (
        "package.json",
        "{\"dependencies\":{\"@angular/core\":\"^19.0.0\",\"@angular/router\":\"^19.0.0\"}}",
    ),
    (
        "src/app/app.routes.ts",
        "import { Routes } from '@angular/router';\nimport { TasksComponent } from './tasks/tasks.component';\nexport const routes: Routes = [{ path: 'tasks', component: TasksComponent }];\n",
    ),
    (
        "src/app/nav/nav.component.ts",
        "import { Component, signal } from '@angular/core';\n@Component({\n  selector: 'app-nav',\n  template: `<nav>\n    <app-theme-toggle [class.open]=\"isOpen()\" />\n  </nav>`,\n})\nexport class NavComponent {\n  isOpen = signal(false);\n}\n",
    ),
    (
        "src/app/theme-toggle/theme-toggle.component.ts",
        "import { Component, computed, signal } from '@angular/core';\n@Component({ selector: 'app-theme-toggle', templateUrl: './theme-toggle.component.html' })\nexport class ThemeToggleComponent {\n  private readonly theme = signal<'light' | 'dark'>('light');\n  icon = computed(() => (this.theme() === 'dark' ? 'moon' : 'sun'));\n  label = computed(() => (this.theme() === 'dark' ? 'Dark' : 'Light'));\n  cycle(): void {\n    this.theme.set(this.theme() === 'dark' ? 'light' : 'dark');\n  }\n  unused(): void {}\n}\n",
    ),
    (
        "src/app/theme-toggle/theme-toggle.component.html",
        "<button (click)=\"cycle()\" [attr.aria-label]=\"label()\">\n  <lucide-icon [name]=\"icon()\"></lucide-icon>\n</button>\n",
    ),
    (
        "src/app/tasks/tasks.component.ts",
        "import { Component, computed, signal } from '@angular/core';\n@Component({ selector: 'app-tasks', templateUrl: './tasks.component.html' })\nexport class TasksComponent {\n  lists = signal<{ id: number; title: string; items: { done: boolean }[] }[]>([]);\n  selectedListId = signal<number | null>(null);\n  selected = computed(() => this.lists().find((l) => l.id === this.selectedListId()));\n  editing = signal(false);\n  title = 'Tasks';\n  form = { reset() {} };\n  days = Array.from({ length: 3 }, (_, i) => i + 1);\n  row = computed(() => 0);\n  nameInput = (value: string) => value;\n\n  get canSave(): boolean {\n    return this.lists().length > 0;\n  }\n  remainingItems(list: { items: { done: boolean }[] }): number {\n    return list.items.filter((i) => !i.done).length;\n  }\n  trackById(index: number, list: { id: number }): number {\n    return list.id;\n  }\n  displayFn(list: { title: string }): string {\n    return list.title;\n  }\n  format(value: string): string {\n    return value;\n  }\n  reset(): void {}\n  set(): void {}\n  legacy(): string {\n    return '';\n  }\n  quoted(): string {\n    return '';\n  }\n  save(): void {}\n}\n",
    ),
    (
        "src/app/tasks/tasks.component.html",
        "<!-- {{ legacy() }} -->\n<h1 [title]=\"title\">{{ 'quoted()' }}</h1>\n<input #nameInput [value]=\"nameInput.value\" (blur)=\"selected() && save()\" />\n@if (editing()) {\n  <span>editing</span>\n}\n<ul>\n  @for (list of lists(); track trackById($index, list)) {\n    <li [class.active]=\"list.id === selectedListId()\" (click)=\"selectedListId.set(list.id)\">\n      {{ list.title | format }} <small>{{ remainingItems(list) }}</small>\n    </li>\n  }\n</ul>\n@for (row of [selected()]; track row) {\n  <b>{{ row?.title }}</b>\n}\n@for (day of days; track day) {\n  <i>{{ day }}</i>\n}\n<mat-autocomplete [displayWith]=\"displayFn\"></mat-autocomplete>\n<button [disabled]=\"!canSave\" (click)=\"form.reset()\">Reset</button>\n",
    ),
];
