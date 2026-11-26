class Signup
  def call(user)
    WelcomeWorker.perform_async(user.id)
  end
end
