# Load Balancer

A load balancer project with adaptive decision engine

## Project Requirements

### Load balancer structure

Receive incoming requests: Use a popular framework like Axum or Hyper.
Define a worker servers list: Maintain a list of backend worker servers and corresponding addresses.
Reroute incoming requests: Distribute incoming requests to worker servers using different load balancing algorithms (e.g., round-robin, least connections).

### Health checks
Implement health check endpoints: Ensure worker servers have a health check endpoint to report their status.
Check health status using the load balancer: Periodically check the health of worker servers.

### Load balancing algorithms
Round-robin: Distribute requests evenly across all worker servers.
Least connections: Route requests to the worker server with the least active connections.

### Adaptive load balancing with decision engine
Real-time data: Continuously collect performance data such as response times and concurrent connections.
Decision engine: Analyze the accumulated data to make decisions about which load balancing algorithm to use.
Adaptive algorithm switching: Dynamically switch between different load balancing algorithms based on the decisions made by the decision engine.

## Project Milestones

### Week 1
- Understand load balancing fundamentals
    - Study fundamentals of distributed systems and load balancing, such as:
    - DNS-based vs proxy-based/application-layer load balancing
    - Software vs hardware balancers
    - Common load balancing algorithms (e.g., round-robin, least connections).
- Go through the following recommended resources:
    - What is a distributed system?
    - Why we need a load balancer
    - Most common load balancing algorithms
    - Create an HTTP proxy server in Rust with hyper
    - This is very close to a load balancer! The additional work involves forwarding requests to a list of worker servers!
- Brainstorm
    - Plan how to apply the Rust concepts and best practices you've learned in the program to this project.
- Experiment
    - Create small Rust programs to get familiar with load balancing algorithms like:
        - Round-robin
        - Least connections
- Notify your instructor
- Inform your instructor on Discord that you have chosen this project.

### Week 2
- Create a worker server
    - Add a health check endpoint
    - Simply return 200 OK for now, indicating a healthy server.
    - Add a work endpoint
    - Return 200 OK after a 10ms delay that simulates work being done.
    - Start up multiple worker server instances
- Create a basic load balancer server
    - Round-robin algorithm
    - Implement a simple round-robin algorithm to distribute requests evenly.
    - Test with worker servers to ensure proper request distribution.
    - Use a client like Postman to send traffic to the load balancer.
    - Additional load balancing algorithms
    - Implement and test at least one other algorithm, such as least connections.
- Manual run-time algorithm switching
    - Implement the ability to switch algorithms at runtime via an endpoint on the load balancer.
    - Test under various conditions to see how each algorithm performs
    - Use a client like Postman to send traffic to the load balancer
    - Update the work endpoint to artificially introduce conditions like prolonged request processing and elevated error rates.
    - Evaluate the performance impact of different load balancing algorithms under various conditions.
- Showcase progress
- Notify your instructor if you’d like to showcase your work on Discord or during a live call.
- Share your progress on Discord and/or during a live call.

### Week 3
- Implement adaptive load balancing
    - Real-time data and conditions
    - Define conditions under which algorithm switches should occur.
    - Start with straightforward conditions that are easy to (re)produce, such as prolonged request processing time or elevated error rates.
    - For now, collect these metrics about worker servers within the load balancer using simple in-memory data structures (e.g., HashMap). No need to use third-party services like Datadog.
    - Prevent overhead and maintain system stability by limiting algorithm switches (e.g., at most one switch per 60-second window).
    - For now, make sure these conditions can be artificially triggered by updating the worker servers.
- Implement decision engine
    - Develop a decision engine to monitor and evaluate the defined conditions.
    - Implement functionality to signal when to switch algorithms based on triggered conditions.
- Adaptive load balancing
    - Transition to using the decision engine for runtime algorithm switching, replacing the manual approach.
    - Monitor the system to ensure the load balancer responds correctly to changing conditions and optimizes performance.
- Showcase Progress
- Notify your instructor if you’d like to showcase your work on Discord or during a live call.
- Share your progress on Discord and/or during a live call.

### Week 4
- Refactoring
- Make sure to incorporate the best patterns and practices we've learned during this program into your projects.
- Bug fixes
- Fix any remaining bugs.
- Optional enhancements 
- Implement any additional features and put the last finishing touches on your project. See the list of optional enhancements below! 
- Demo prep
- Prepare to demo your work at the final capstone demo day! 

### Optional Enhancements

External observability service
Capture more extensive metrics using an external observability service like Datadog.
Enhance the decision-making of the decision engine by monitoring more sophisticated metrics captured by this external service.

Fault tolerance
Ensure the load balancer continues to operate correctly during partial or complete failures.
Implement redundancy and failover mechanisms to handle load balancer failures gracefully.

Deployment
Containerize the load balancer and backend servers with Docker for consistent deployment.
Integrate with a container orchestrator like Kubernetes to manage the load balancer and backend services as needed.

(Note that these enhancements are optional and should only be attempted after the core load balancer functionality is working.)
