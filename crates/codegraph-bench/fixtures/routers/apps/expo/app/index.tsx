import { router } from 'expo-router';
import { Button } from 'react-native';

export default function Index() {
  return <Button title="Settings" onPress={() => router.push('/settings')} />;
}
