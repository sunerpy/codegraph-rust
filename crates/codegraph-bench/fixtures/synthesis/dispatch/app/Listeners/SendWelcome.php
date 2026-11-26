<?php
namespace App\Listeners;

use App\Events\Registered;

class SendWelcome
{
    public function handle(Registered $event): void
    {
    }
}
